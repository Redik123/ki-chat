# Audit ki-chat — rapport de bugs

**Date :** 2026-08-17 · **Version auditée :** 0.1.3 (`ded5af7`)
**Méthode :** lecture intégrale des 8 crates par 7 revues croisées (modèle Opus), puis
vérification directe dans le code des points les plus graves. `cargo test` (61 tests) et
`cargo clippy` passent — les bugs ci-dessous ne sont donc **pas** attrapés par la CI actuelle.

> **État au 2026-08-17 — branche `fix/audit-critiques` :** **les six critiques C1 à C6
> sont corrigés**, plus **M7** (la pagination qui bouclait et faisait sauter l'écran) et
> **M8** pour l'historique (le salon voyage désormais avec la page ; `Chat` ne le porte
> toujours pas, mais il arrive dans l'ordre du flux, donc sans ce risque).
> 14 commits, 10 tests de non-régression ajoutés, 120 tests au vert sur l'espace de
> travail, et le démarrage du serveur vérifié en conditions réelles — journal
> volontairement corrompu compris.
>
> **Six défauts ont été trouvés dans les correctifs eux-mêmes, en les relisant**, et
> corrigés dans la foulée : un garde-fou d'un octet trop permissif ; une course sur le
> fichier temporaire de `write_atomic` ; une fenêtre pendant laquelle un bannissement
> prononcé durant la vérification du mot de passe ne s'appliquait pas ; un limiteur de
> débit qui fermait la session, et aurait donc déconnecté quiconque remontait simplement
> une conversation ; un dépôt d'échantillons non borné qui remettait une allocation sur
> le chemin du fil temps réel ; et un recalage de défilement non borné qui envoyait la
> vue au-delà de la fin du fil.
>
> **Lot modération (branche `fix/moderation`) :** **M1 à M6 sont corrigés** — les quatre
> permissions « membre » sont enfin appliquées, `@everyone` devient modifiable pour
> qu'elles puissent mordre, un changement de rôle est poussé à l'intéressé, et les trois
> gardes d'autorisation manquantes sont posées.
>
> **Lot vocal (branche `fix/vocal`) :** **M11 à M18 sont corrigés** — nonce de
> chiffrement, trames escamotées, tampon de gigue, moteur orphelin, périphérique de
> repli, bornes du relais, et l'entrée en vocal enfin réconciliée avec le serveur.
>
> **Lot transport :** **M22 et M23 sont corrigés** — le partage de fichiers passe en
> TLS, et le client vérifie l'identité du serveur par empreinte mémorisée à la première
> connexion. La relecture y a trouvé deux failles, dont une qui rendait toute la
> vérification décorative : les signatures de poignée de main étaient acceptées sans
> être validées, ce qui laissait rejouer le certificat public sans en posséder la clé.
>
> Tous les autres majeurs et mineurs ci-dessous **ne sont pas corrigés**.

**Verdict :** le socle est soigné (validation d'entrées, compatibilité de versions, tests
sérieux) mais il reste des défauts qui, en usage réel, cassent le service ou une
fonctionnalité visible. Les plus graves tiennent en une phrase : **le serveur peut refuser
de redémarrer, se figer ou saturer sa mémoire à cause d'un seul client ou d'un fichier
abîmé**, et **plusieurs réglages d'administration ou d'audio ne font rien de ce qu'ils
annoncent**.

Légende : ✅ = vérifié par lecture directe du code · 🔎 = rapporté avec confiance élevée,
mécanisme cohérent avec le code lu.

---

## CRITIQUES — cassent le service, l'audio, ou les données

### ✅ CORRIGÉ — C1 ✅ Une seule ligne abîmée dans un journal de salon empêche le serveur de redémarrer, définitivement
[`crates/server/src/history.rs:49`](crates/server/src/history.rs:49)
`let line = line?;` **propage** l'erreur : dès qu'une ligne de `channel-N.jsonl` n'est pas
de l'UTF-8 valide (fin de fichier tronquée par un disque plein, une coupure secteur, une
sauvegarde partielle), `History::open` échoue, donc `AppState::new`, donc le démarrage. Or
la ligne suivante montre l'intention inverse (le JSON invalide, lui, est ignoré), et les
deux autres lecteurs du même format tolèrent la casse ([`audit.rs:48`](crates/server/src/audit.rs:48)
`let Ok(line) = line else { continue }`, [`history.rs:180`](crates/server/src/history.rs:180)
`map_while(Result::ok)`). Trois comportements pour un même format : c'est un oubli.
**Correctif :** `for line in reader.lines().map_while(Result::ok)` + sauter les lignes non désérialisables.

### ✅ CORRIGÉ — C2 ✅ `write_atomic` ne synchronise rien : l'atomicité promise ne survit pas à une coupure de courant
[`crates/server/src/store.rs:18`](crates/server/src/store.rs:18)
`std::fs::write(&tmp, bytes)?` puis `std::fs::rename(...)` — **aucun `sync_all()`** sur le
temporaire avant le renommage, ni sur le répertoire après. Le `rename` est atomique côté
*métadonnées*, mais rien ne garantit que les *données* du temporaire soient sur le disque
avant le commit du renommage. Une coupure secteur peut laisser `users.json`, `roles.json`
ou `channels.json` remplis de zéros — et `Roles::open`/`Accounts::open` refusent alors de
démarrer (choix assumé, mais sans repli). L'en-tête du module promet pourtant « jamais un
fichier à moitié écrit » ; c'est vrai pour un crash process, faux pour une coupure secteur.
**Correctif :** `File::sync_all()` sur le tmp avant `rename`, idéalement `fsync` du dossier après.

### ✅ CORRIGÉ — C3 ✅ Salon « piège » : une réponse `History` trop grosse déconnecte en boucle (trouvé par 2 revues)
Émission : [`crates/server/src/quic.rs:659`](crates/server/src/quic.rs:659) ·
Réception : [`crates/client-quic/src/lib.rs:198`](crates/client-quic/src/lib.rs:198)
`MAX_LINE` (160 Kio) est appliqué **en lecture** des deux côtés, mais **jamais en écriture**.
`History` renvoie jusqu'à 1000 messages, chacun jusqu'à `MAX_CHAT_TEXT` = 4000 **caractères**.
Les deux bornes n'ont jamais été confrontées : **~41 messages ASCII longs (ou ~11 messages
de 4000 émojis)** dépassent 160 Kio. Côté client, `read_line` voit la ligne trop longue et
renvoie `None`, que le client traduit en « connexion fermée » → déconnexion. Comme `Welcome`
ouvre d'office le premier salon textuel, si c'est ce salon qui est trop lourd, **la connexion
échoue en boucle et l'application est inutilisable** jusqu'à édition manuelle du `.jsonl`.
Mêmes vecteurs : `AdminInfo` (toutes les invitations jamais purgées) et `AuditLog`.
**Correctif :** paginer/borner la taille des réponses serveur, ou monter `MAX_LINE`, ou fragmenter `History`.

### ✅ CORRIGÉ — C4 ✅ Le verrou global des comptes est tenu pendant tout un Argon2, et réclamé depuis la boucle async → gel du serveur (voix comprise)
[`crates/server/src/accounts.rs:224`](crates/server/src/accounts.rs:224) (lock) →
[`:252`](crates/server/src/accounts.rs:252) (`verify_password` **sous le lock**) ·
[`crates/server/src/state.rs:404`](crates/server/src/state.rs:404) (`roster()` → `avatar_hashes()` reprend le même lock)
`authenticate` garde `self.inner.lock()` à travers `Argon2::verify_password` (50–200 ms,
19 Mio). Le hachage est bien sur `spawn_blocking`, mais **le verrou ne l'est pas**. Ce même
verrou est réclamé sur le chemin chaud : `roster()` (appelé à chaque join/leave vocal,
connexion, déconnexion) prend le lock des comptes et recalcule le FNV de toutes les photos.
Sur un VPS 2 vCPU, quelques (re)connexions simultanées suffisent à figer la boucle tokio —
donc le relais des datagrammes vocaux. Aggravant : `refresh_member` tient le lock `users`
*pendant* qu'il attend le lock `accounts` ([`state.rs:264`](crates/server/src/state.rs:264)).
**Correctif :** cloner le hash sous le lock, relâcher, puis `verify_password` hors lock.

### ✅ CORRIGÉ — C5 ✅ File d'envoi par client non bornée + aucune limite de débit après authentification → mémoire du serveur épuisée par un seul membre (trouvé par 3 revues)
[`crates/server/src/quic.rs:225`](crates/server/src/quic.rs:225) (`unbounded_channel`) ·
[`:960`](crates/server/src/quic.rs:960) (`RequestAvatars` sans dédoublonnage) ·
[`:654`](crates/server/src/quic.rs:654) (`History`)
Seul `Chat` a un seau à jetons. Un client authentifié qui **cesse de lire son flux** puis
spamme `History{limit:1000}` ou `RequestAvatars` (64× le même id → 64× jusqu'à 96 Kio)
empile des Mio dans une file qui ne se vide jamais (`write_all` est bloqué par le contrôle
de flux QUIC). Quelques secondes → plusieurs Gio → OOM. `ChangePassword` en est un autre
amplificateur (deux Argon2 de 19 Mio par message, cf. M26).
**Correctif :** canal borné (drop ou déconnexion si plein) + seau à jetons global par connexion.

### ✅ CORRIGÉ — C6 🔎 Le décodage Opus (PLC neuronal, DRED) tourne sous le mutex dont dépend le callback audio de sortie
[`crates/voice/src/lib.rs:538`](crates/voice/src/lib.rs:538) (lock + `rx.push` lourd) ·
[`:1242`](crates/voice/src/lib.rs:1242) (callback bloqué sur le même lock) ·
[`crates/voice/src/jitter.rs:141`](crates/voice/src/jitter.rs:141) (drain jusqu'à ~200 trames)
`recv_loop` prend `receivers.lock()` **puis** décode (Opus complexité 10 = Deep PLC neuronal,
+ DRED). La boucle de drain peut générer jusqu'à ~200 trames PLC d'affilée, verrou tenu.
Pendant ce temps le callback cpal de sortie (thread temps réel) se bloque sur le même verrou.
Résultat : **craquements et coupures précisément quand le réseau se dégrade** — le pire moment.
**Correctif :** décoder hors du verrou (préparer les échantillons, ne verrouiller que l'écriture).

---

## MAJEURS — fonctionnalité visiblement cassée en usage normal

### Permissions & rôles

- **✅ CORRIGÉ — M1 ✅ Quatre permissions ne sont vérifiées nulle part** (trouvé par 3 revues).
  `SEND_MESSAGE`, `CONNECT_VOICE`, `UPLOAD_FILE`, `VIEW_CHANNEL` n'apparaissent dans aucun
  contrôle du serveur (grep exhaustif : une seule occurrence, dans un test). Les cases
  « Écrire / Rejoindre le vocal / Partager / Voir » de l'éditeur de rôles **ne font rien** :
  `Chat` ne vérifie que la visibilité + l'anti-spam, `JoinVoice` que la nature du salon +
  la visibilité, `upload` que le jeton. Pire, le modèle les rend inopérantes même si on les
  vérifiait : `perms_of` est une *union* incluant `@everyone`, rôle système dont les perms
  sont figées → impossible de retirer `perm::DEFAULT` à qui que ce soit.
  [`quic.rs:615`](crates/server/src/quic.rs:615), [`:572`](crates/server/src/quic.rs:572),
  [`files.rs:85`](crates/server/src/files.rs:85). **Correctif : demande un vrai modèle de
  « deny » par salon (surcharge à la Discord), pas juste un `require` en plus.**

- **✅ CORRIGÉ — M2 🔎 Un changement de rôle n'est jamais repoussé à l'intéressé.** `perms`/`rank` ne
  voyagent que dans `Welcome`, envoyé une seule fois. On promeut quelqu'un modérateur : chez
  lui, aucun bouton n'apparaît jusqu'au redémarrage de l'appli ; on le rétrograde : ses
  boutons restent et échouent tous. [`quic.rs:1247`](crates/server/src/quic.rs:1247),
  [`client-gui/src/main.rs:1261`](crates/client-gui/src/main.rs:1261).
  **Correctif :** un `ServerMsg` qui pousse perms/rank, ou recalcul client depuis `Members`+`Roles`.

- **✅ CORRIGÉ — M3 ✅ Escalade : `AdminSetUserRoles` n'applique pas `grantable`.**
  [`quic.rs:1067`](crates/server/src/quic.rs:1067). Contrairement à `AdminCreateRole`/`EditRole`,
  la seule garde est le rang. Un porteur de `MANAGE_ROLES` peut attribuer à un compte de rang
  inférieur un rôle portant des permissions **qu'il n'a pas lui-même** (ex. le « Modérateur »
  par défaut, rang 100, `KICK|BAN`). **Correctif :** passer les perms des rôles visés par `grantable`.

- **✅ CORRIGÉ — M4 ✅ `AdminUnban` ne vérifie pas le rang.** [`quic.rs:882`](crates/server/src/quic.rs:882).
  Bannir exige `BAN` **et** de surclasser la cible ; débannir n'exige que `BAN`. Un modérateur
  peut donc lever un bannissement posé par le propriétaire. **Correctif :** ajouter `outranks_account`.

- **✅ CORRIGÉ — M5 🔎 Panneau admin (onglet Membres) : boutons sans garde, et on peut se bannir soi-même.**
  [`client-gui/src/main.rs:4111`](crates/client-gui/src/main.rs:4111). « Bannir… » et
  « Réinitialiser le mot de passe » s'affichent sans vérifier `BAN`/`RESET_PASSWORD`, sans
  `outranks`, et sans exclure soi-même — les clics échouent côté serveur, mais l'UI ment.
  Contredit le principe posé ailleurs (« chaque action n'apparaît que si elle aboutirait »).

- **✅ CORRIGÉ — M6 🔎 Modifier un rôle portant une perm qu'on n'a pas est refusé en bloc — même pour un
  simple renommage.** [`quic.rs:1024`](crates/server/src/quic.rs:1024) applique `grantable`
  au masque entier, mais le GUI **masque** les cases hors de portée tout en renvoyant le masque
  complet ([`main.rs:3829`](crates/client-gui/src/main.rs:3829)). Rien à décocher, donc rôle
  immodifiable. **Correctif :** ne comparer que les bits *changés*, ou préserver les bits masqués sans les re-soumettre.

- **M25 🔎 `send_admin_info` fuite tous les codes d'invitation à 6 permissions différentes**
  (dont `KICK` seul). [`quic.rs:479`](crates/server/src/quic.rs:479). Comptes + invitations
  voyagent dans le même message ; un modérateur relève un lien permanent et ouvre le serveur
  privé. **Correctif :** filtrer le contenu d'`AdminInfo` par permission du destinataire.

### Historique, pagination, chat

- **M7 ✅ CORRIGÉ — Remonter le fil charge tout l'historique d'un coup, en boucle** (trouvé par 2 revues).
  [`main.rs:2738`](crates/client-gui/src/main.rs:2738). `offset.y <= 24` déclenche une page ;
  `HistoryPage` **préfixe** sans compenser le décalage → la vue reste collée au sommet → la
  condition reste vraie à l'image suivante → une page toutes les 50 ms jusqu'à épuisement.
  Aucun plafond sur `HistoryPage` (le plafond de 500 n'existe que sur `Chat`) → RAM sans borne.
  **Correctif :** compenser le scroll après préfixage + ne redéclencher que sur interaction réelle.

- **M8 ✅ CORRIGÉ (pour l'historique) — `Chat`/`History`/`HistoryPage` ne portent pas l'id du salon → page dans le mauvais salon**
  (trouvé par 3 revues). [`protocol/src/lib.rs:301`](crates/protocol/src/lib.rs:301),
  [`quic.rs:662`](crates/server/src/quic.rs:662) (réponse hors-ordre via `spawn_blocking`),
  [`main.rs:1315`](crates/client-gui/src/main.rs:1315) (préfixage aveugle). On remonte A, on
  clique B pendant la lecture disque : la page de A se colle en haut de B, et écrase le drapeau
  `history_more` de B. **Correctif :** ajouter `channel` à ces messages, filtrer côté client.

- **M9 🔎 Un message envoyé pendant une coupure disparaît sans le dire** (trouvé par 2 revues).
  [`main.rs:2650`](crates/client-gui/src/main.rs:2650). `send(...)` puis `input.clear()` :
  si le flux est cassé (`let _ = ...`), le texte est perdu, aucune restitution, aucun accusé.
  **Correctif :** ne vider le champ qu'après confirmation, ou remettre le texte en cas d'échec.

- **M30 ✅/🔎 Pagination : deux messages de la même milliseconde sont perdus (ou fusionnés).**
  Le curseur est `before_ts` avec filtre strict `ts < before_ts`, mais les fenêtres sont
  découpées par comptage : si la coupure tombe entre deux messages de même `ts`, le jumeau est
  exclu définitivement ([`history.rs:157`](crates/server/src/history.rs:157)). Côté client, la
  dédup `(user_id, ts)` fusionne deux messages du même auteur à la même ms
  ([`main.rs:1324`](crates/client-gui/src/main.rs:1324)). `ChatRecord` n'a pas d'id de message
  → limite de contrat autant que bug. **Correctif :** clé d'ordre secondaire (id de message).

### Vocal

- **✅ CORRIGÉ — M11 ✅ Entrée en vocal optimiste, jamais réconciliée** (trouvé par 3 revues).
  [`main.rs:938`](crates/client-gui/src/main.rs:938). `join_voice` pose `voice_channel` et
  arme le micro **avant** toute réponse ; un `Error` serveur ne le retire pas (seul `VoiceLocked`
  le fait). Résultat : « je suis affiché dans le salon, mon micro s'allume, personne ne
  m'entend ». **Correctif :** n'armer qu'après confirmation, et recaler sur `Member.voice`.

- **✅ CORRIGÉ — M12 🔎 Moteur audio orphelin (pas de `Drop`) sur 2ᵉ `Welcome` ou `restart_voice` concurrent.**
  [`net.rs:295`](crates/client-gui/src/net.rs:295), [`voice/src/lib.rs:250`](crates/voice/src/lib.rs:250).
  `*engine_slot = Some(engine)` écrase l'ancien, simplement droppé : ses threads audio gardent
  un `Arc<Shared>` avec `shutdown == false` et tournent pour toujours (double voix, CPU qui monte).
  **Correctif :** `impl Drop for VoiceEngine` qui appelle `shutdown`, ou `take()`+`shutdown` avant remplacement.

- **✅ CORRIGÉ — M13 🔎 Le périphérique choisi est remplacé en silence par le défaut, jamais repris.**
  [`voice/src/lib.rs:132`](crates/voice/src/lib.rs:132). Micro/casque introuvable → `warn!` +
  repli sur le défaut, `input_lost = false` (donc **aucune bannière**), et comme le repli
  fonctionne on ne re-teste jamais le retour du vrai périphérique. C'est exactement le cas que
  le commit « rebrancher un micro ne casse plus le vocal » n'a pas couvert. **Correctif :**
  re-tenter périodiquement le périphérique nommé, signaler le repli.

- **✅ CORRIGÉ — M14 🔎 Une rafale `JoinVoice`/`LeaveVoice`, ou le relais voix sans plafond, étrangle le
  vocal de tous.** [`quic.rs:572`](crates/server/src/quic.rs:572), [`:321`](crates/server/src/quic.rs:321).
  Chaque bascule prend le verrou d'écriture de `voice_routes` (celui que chaque datagramme lit)
  + un `roster()` complet diffusé à tous. Et le relais ne borne ni la cadence ni la taille
  (`VOICE_MAX_PACKET` n'est pas vérifié côté serveur). Un membre sature la liaison montante ×(N-1).
  **Correctif :** limiter le débit de ces messages, borner la taille des datagrammes relayés.

- **✅ CORRIGÉ — M15 ✅ Réutilisation de nonce XChaCha20 (compteur remis à 1, clé inchangée).**
  [`voice/src/lib.rs:625`](crates/voice/src/lib.rs:625). Le nonce est `(user_id, counter)` ;
  `Sender::new` repart de `counter: 1`, alors que la clé ne change qu'au **redémarrage du
  serveur**. Le rebranchement à chaud est protégé, mais `restart_voice` (changement de micro
  dans les réglages) et toute reconnexion recréent le moteur → nonce réutilisé. Le commentaire
  du code décrit lui-même ce danger. *Impact atténué* par le tunnel QUIC (un tiers externe ne
  voit pas les datagrammes en clair) et par la vérif `user_id` serveur (pas d'usurpation d'émetteur) ;
  mais la promesse « chiffré de bout en bout » est cassée pour de bon. **Correctif :** persister
  le compteur à travers les redémarrages du moteur, ou dériver une clé par session.

- **✅ CORRIGÉ — M16 🔎 Le tampon de gigue confond silences PTT/VAD et gigue réseau** →
  [`jitter.rs:111`](crates/voice/src/jitter.rs:111). L'émetteur cesse d'émettre en silence ;
  l'inter-arrivée après une pause de 2 s est lue comme 2000 ms de gigue → ~160 ms avalés au
  début de chaque phrase, même sur LAN. **Correctif :** ignorer les inter-arrivées qui suivent un silence connu.

- **✅ CORRIGÉ — M17 🔎 La latence du jitter monte et ne redescend jamais sous ~140 ms.**
  [`jitter.rs:167`](crates/voice/src/jitter.rs:167). Le rognage anti-dérive ne s'active qu'à
  7+ trames ; rien ne ramène le tampon vers sa cible (2 trames). Chaque micro-perte l'inflate,
  jamais l'inverse. **Correctif :** cible de latence adaptative qui redescend en régime propre.

- **✅ CORRIGÉ — M18 ✅ Trames Opus > 1365 octets : jetées, mais sans trou de séquence** →
  [`voice/src/lib.rs:648`](crates/voice/src/lib.rs:648). Le `return` précède l'incrément du
  compteur : pas de paquet émis **et** pas de trou → le récepteur n'applique ni FEC ni PLC,
  20 ms disparaissent, et toutes les stats affichent 0 % de perte. Déclenché par un transitoire
  à haut débit avec DRED engagé. **Correctif :** émettre quand même (tronquer/renuméroter) ou incrémenter le compteur.

### Connexion & état client

- **M19 🔎 « Connexion… » peut rester bloqué indéfiniment**, sans délai ni annulation.
  [`main.rs:994`](crates/client-gui/src/main.rs:994). `connecting` n'est levé que par
  `Welcome`/`ConnectFailed`/`Disconnected` ; rien ne borne l'attente du `Welcome`, et le
  keep-alive de 5 s empêche le timeout d'inactivité de 30 s de jouer. **Correctif :** timeout sur le `Welcome` + bouton d'annulation.

- **M20 🔎 Le motif de kick/ban n'arrive jamais chez l'intéressé.**
  [`quic.rs:724`](crates/server/src/quic.rs:724). `t.send(Kicked{reason})` ne fait que mettre
  en file ; `disconnect` ferme la connexion QUIC dans la foulée (`close` = « no more data sent »).
  Le client voit une déconnexion muette. **Correctif :** attendre le flush du message avant de fermer (petit délai ou drain explicite).

- **M21 🔎 `disconnect()` client ne réinitialise pas tout l'état du serveur précédent** (2 revues).
  [`main.rs:969`](crates/client-gui/src/main.rs:969). `avatars`, `admin_users`, `admin_invites`,
  `audit`, `server_info`, `ban_draft`, `voice_prompt`, `history_more`… survivent. Les `user_id`
  étant par-serveur, on voit la photo de l'utilisateur 3 du serveur A pour celui du serveur B ;
  une fenêtre « Bannir toto » ressurgit sur un autre serveur. **Correctif :** réinitialisation exhaustive (ou reconstruire `KiApp` par connexion).

### Sécurité transport & DoS

- **✅ CORRIGÉ — M22 ✅ Fichiers partagés et jeton de session transitent en HTTP clair** (3 revues).
  [`main.rs:814`](crates/client-gui/src/main.rs:814), [`files.rs:85`](crates/server/src/files.rs:85).
  Tout le reste est dans le tunnel QUIC/TLS ; l'upload/download HTTP, lui, expose le contenu
  (y compris salons privés) et le `x-ki-token` — qui permet ensuite d'uploader sous l'identité
  de la victime. **Correctif :** servir les fichiers dans le même transport chiffré, ou TLS sur le port 8080.

- **✅ CORRIGÉ — M23 ✅ Le certificat serveur n'est pas vérifié du tout, et l'argument du code est faux.**
  [`client-quic/src/lib.rs:43`](crates/client-quic/src/lib.rs:43). `SkipVerify` accepte tout,
  sans TOFU ni épinglage. Le commentaire justifie par « la voix a son propre chiffrement de
  bout en bout » — faux, la clé voix est distribuée par le serveur dans `Welcome`. Un MITM
  actif (Wi-Fi public, DNS menteur) lit le **mot de passe en clair** du premier `Auth`.
  Choix partiellement assumé (serveur privé auto-signé), mais l'absence de TOFU le rend évitable.
  **Correctif :** TOFU — mémoriser l'empreinte à la 1ʳᵉ connexion, alerter si elle change.

- **M24 🔎 `accept_bi()` sans timeout : connexions jamais authentifiées gardées indéfiniment.**
  [`quic.rs:115`](crates/server/src/quic.rs:115). Le délai de 10 s ne couvre que la 1ʳᵉ ligne,
  pas l'ouverture du flux ; les keep-alives empêchent l'idle-timeout de jouer. Des dizaines de
  milliers de connexions parquées sans authentification. **Correctif :** timeout autour de `accept_bi`, plafond de connexions par IP.

- **M26 ✅ `ChangePassword` hache le nouveau mot de passe (Argon2) avant de vérifier l'ancien,
  sans limite de débit.** [`accounts.rs:450`](crates/server/src/accounts.rs:450). Amplificateur
  DoS (cf. C5) : chaque message coûte un Argon2 complet même avec un ancien mot de passe faux.
  **Correctif :** vérifier l'ancien d'abord + soumettre ce chemin au rate-limit.

### Persistance

- **M27 ✅ `save()` avale les échecs disque** → bans/rôles/salons « réussis » à l'écran mais
  jamais persistés. [`accounts.rs:601`](crates/server/src/accounts.rs:601),
  [`roles.rs:297`](crates/server/src/roles.rs:297), [`channels.rs:298`](crates/server/src/channels.rs:298).
  `if let Err(e) = write_atomic(...) { tracing::error!(...) }` — l'appelant renvoie `Ok(())` et
  l'UI confirme le succès. Disque plein → un ban tient jusqu'au redémarrage puis disparaît.
  **Correctif :** propager l'erreur jusqu'au client.

- **M28 ✅ Écritures d'état de la phase 2 et tous les `audit.record` restés bloquants dans la
  boucle async.** [`quic.rs:1055`](crates/server/src/quic.rs:1055),
  [`:1082`](crates/server/src/quic.rs:1082), et les `audit.record` hors `spawn_blocking`.
  `set_roles`/`remove_role` réécrivent `users.json` (photos base64 comprises, ~Mo) et chaque
  `audit.record` fait un `writeln!` synchrone — le vocal de tous hoquette à chaque action admin.
  C'est le symptôme que le commit « I/O hors de la boucle asynchrone » disait avoir soldé, mais
  la phase 2 l'a réintroduit. **Correctif :** `spawn_blocking` sur ces chemins aussi.

- **M29 ✅ `ServerMeta::update` écrit hors du verrou, et deux écritures partagent le même
  `server.tmp`.** [`meta.rs:44`](crates/server/src/meta.rs:44). Seul magasin à relâcher le
  verrou avant d'écrire ; deux admins simultanés → mise à jour perdue, ou `server.tmp` entrelacé
  → identité du serveur qui repart à vide au redémarrage. **Correctif :** écrire sous le verrou, tmp unique par écriture.

- **M31 🔎 `audit.jsonl` n'est pivoté qu'au démarrage** → grossit sans limite sur un serveur qui
  ne redémarre pas, et les archives ne sont jamais purgées. [`audit.rs:42`](crates/server/src/audit.rs:42).
  Un disque plein déclenche C1 et M27. **Correctif :** vérifier la taille à l'écriture, purger les vieilles archives.

### Performance UI (client)

- **M32 🔎 Aperçus d'images : tempête de threads/téléchargements dès ~41 images, fuite VRAM.**
  [`images.rs:105`](crates/client-gui/src/images.rs:105), [`main.rs:4869`](crates/client-gui/src/main.rs:4869).
  `image_preview` est appelé pour **chaque** message (visible ou non) ; le cache (40 entrées)
  évince des entrées encore en `Loading` → réinsertion → `thread::spawn` de plus, à 20 fps.
  `mount` réinsère hors de la liste d'éviction → textures accumulées sans borne. **Correctif :**
  ne charger que le visible, ne pas évincer les `Loading`, corriger la comptabilité du cache.

- **M33 🔎 Images du chat décodées sur le thread UI**, toutes dans la même image de rendu (jusqu'à
  12 Mo / 8000 px chacune). [`images.rs:86`](crates/client-gui/src/images.rs:86). Gel de plusieurs
  secondes à l'arrivée de photos. **Correctif :** décoder dans le thread de téléchargement.

- **M34 🔎 Texture d'aperçu unique partagée entre logo serveur et avatar** → re-décodage PNG +
  upload GPU à chaque frame quand Admin▸Serveur et « Mon compte » sont ouverts ensemble.
  [`main.rs:3543`](crates/client-gui/src/main.rs:3543). **Correctif :** deux entrées de cache distinctes.

---

## MINEURS — cas limites, confort, latents

**Client / UI**
- Renommer un salon, le réordonner, poser un mot de passe vocal : **absents du GUI** (seulement en CLI) — [`main.rs:3716`](crates/client-gui/src/main.rs:3716).
- Saisie **mono-ligne** : Maj+Entrée envoie au lieu d'aller à la ligne, alors que le protocole gère le multi-ligne — [`main.rs:2627`](crates/client-gui/src/main.rs:2627).
- Échap ferme une fenêtre par ordre fixe, pas celle du dessus, et ignore les 2 modales — [`main.rs:5113`](crates/client-gui/src/main.rs:5113).
- La fenêtre « Salon verrouillé » **vole le focus clavier à chaque image** (impossible d'écrire dans le chat) — [`main.rs:1958`](crates/client-gui/src/main.rs:1958).
- Pseudos/noms de rôles/salons longs : **débordent** (pas de troncature) — [`main.rs:4740`](crates/client-gui/src/main.rs:4740).
- `Chat` en direct affiché **sans `safe_display`** (l'historique, lui, est nettoyé) — défense en profondeur manquante, 3 revues — [`main.rs:1302`](crates/client-gui/src/main.rs:1302).
- Noms de rôles/salons/comptes du panneau admin affichés sans `safe_display` — [`main.rs:3810`](crates/client-gui/src/main.rs:3810).
- Régler l'**opacité** dans le sélecteur de couleur d'un rôle assombrit la couleur (prémultiplication non défaite) — [`main.rs:3898`](crates/client-gui/src/main.rs:3898).
- « connecté » écrit dans la **couleur des bordures** → illisible (contraste ~1,6:1) — [`main.rs:2312`](crates/client-gui/src/main.rs:2312).
- `request_repaint_after(50 ms)` **inconditionnel** (batterie/ventilateur même à l'écran de connexion) ; PTT échantillonné à 20 Hz rate les pressions brèves — [`main.rs:5129`](crates/client-gui/src/main.rs:5129).
- Au-delà de 500 messages, `messages.remove(0)` efface le message qu'on lit après pagination — [`main.rs:1308`](crates/client-gui/src/main.rs:1308).
- `disconnect()` bloque l'UI ~800 ms ; `restart_voice` la fige ~200 ms (verrou tenu pendant `shutdown`) — [`net.rs:128`](crates/client-gui/src/net.rs:128).
- Une erreur périmée devient le motif affiché de la déconnexion — [`main.rs:1226`](crates/client-gui/src/main.rs:1226).
- `http_base` casse sur IPv6 littéral / hôte sans port — [`main.rs:814`](crates/client-gui/src/main.rs:814), [`client-quic/src/lib.rs:221`](crates/client-quic/src/lib.rs:221).
- Vignette structurellement valide mais indécodable : redemandée indéfiniment (échec non mémorisé) — [`main.rs:1041`](crates/client-gui/src/main.rs:1041).
- Deux instances du client écrasent le carnet de serveurs (pas de verrou d'instance) — [`servers.rs:133`](crates/client-gui/src/servers.rs:133).
- Mise à jour auto : `download` **sans timeout**, et **aucune somme de contrôle/signature** au-delà de TLS — [`update.rs:239`](crates/client-gui/src/update.rs:239).

**Serveur**
- Limiteur d'auth **aveugle aux essais simultanés** (check en lecture, record après Argon2) ; `record_success` efface aussi la clé par IP — [`throttle.rs:74`](crates/server/src/throttle.rs:74). *(Rend C4/C5 plus faciles à déclencher.)*
- Course entre le test de session existante et l'insertion → session fantôme jamais nettoyée — [`quic.rs:216`](crates/server/src/quic.rs:216).
- Compteur de pertes voix : **débordement** possible (panique en debug, `loss_pct` faux en release) ; aucun anti-rejeu applicatif — [`quic.rs:328`](crates/server/src/quic.rs:328).
- `duration_secs`/`ttl_secs` non bornés (`AdminBan`, `AdminCreateInvite`) → ban/invite qui expire immédiatement en release — [`quic.rs:826`](crates/server/src/quic.rs:826).
- Verrou vocal expiré : le cadenas reste affiché (pas de `push_channels` à l'expiration) et un mauvais mot de passe entre quand même — [`state.rs:340`](crates/server/src/state.rs:340).
- Salons vocaux privés **devinables** via le roster (`Member.voice` diffusé globalement) — [`state.rs:414`](crates/server/src/state.rs:414).
- Pseudos **non filtrés** des commandes bidi Unicode (contrairement aux messages) → panneau admin maquillé — [`quic.rs:138`](crates/server/src/quic.rs:138).
- `next_id` des comptes/rôles **non reconsolidé** au boot (contrairement aux salons) → id recyclé après restauration partielle — [`accounts.rs:296`](crates/server/src/accounts.rs:296), [`roles.rs:118`](crates/server/src/roles.rs:118).
- `before()` **tronque** une page à la 1ʳᵉ ligne illisible (`map_while` au lieu de `filter_map`) — [`history.rs:180`](crates/server/src/history.rs:180).
- Course sur le quota de fichiers (lecture + écriture non atomiques) ; `AdminResetPassword` ne coupe pas la session en cours de la cible — [`files.rs:106`](crates/server/src/files.rs:106), [`quic.rs:790`](crates/server/src/quic.rs:790).
- `AdminEditChannel` (remplacement complet) : `allowed_roles` absent = salon rendu **public** ; `position` par défaut = salon remonté en tête (latent, aucun client ne l'envoie) — [`channels.rs:194`](crates/server/src/channels.rs:194).
- Horloge murale qui recule (NTP, VM) mélange/duplique l'historique paginé — [`state.rs:468`](crates/server/src/state.rs:468).
- Rien n'empêche 2 serveurs de partager le même `data/` (pas de verrou) — [`main.rs:57`](crates/server/src/main.rs:57).
- Noms de fichiers réservés Windows (`NUL`, `CON`…) non filtrés (portée faible : serveur Linux) — [`files.rs:65`](crates/server/src/files.rs:65).

**Audio / vidéo**
- Décodeur Opus + état DRED recréés **sous le mutex du callback** à chaque reprise de parole après 5 s de silence — [`voice/src/lib.rs:539`](crates/voice/src/lib.rs:539).
- Trames bloquées dans `pending` en fin de phrase, rejouées collées à la phrase suivante (« il bafouille ») — [`jitter.rs:141`](crates/voice/src/jitter.rs:141).
- Sortie DeepFilterNet **non bornée** (NaN/dépassements vont à l'encodeur) ; modèle chargé sur le thread de capture (saut d'1–3 s au basculement) — [`voice/src/lib.rs:990`](crates/voice/src/lib.rs:990), [`:1014`](crates/voice/src/lib.rs:1014).
- Allocations + 4 verrous dans le callback de sortie ; canal d'entrée non borné — [`voice/src/lib.rs:1233`](crates/voice/src/lib.rs:1233).
- Mort silencieuse du thread de capture (`Err(_) => break` au lieu de `continue`, + effacement de l'alerte) — [`voice/src/lib.rs:734`](crates/voice/src/lib.rs:734).
- Panique dans le thread réseau → **mutex empoisonné** → tue le moteur *et* l'UI — [`jitter.rs:52`](crates/voice/src/jitter.rs:52).
- `ki-opus` : `frame_size` déduit de `pcm.len()`, faux dès qu'on passera en stéréo (latent) — [`ki-opus/src/lib.rs:185`](crates/ki-opus/src/lib.rs:185).
- Vidéo : recyclage des tampons 8 Mio défait par chaque trame sautée (cas dominant en 1080p) ; RGBA réalloué par trame ; pipeline s'arrête sur erreur pendant que la capture tourne — [`video/src/capture.rs:69`](crates/video/src/capture.rs:69), [`video/src/lib.rs:95`](crates/video/src/lib.rs:95).

**CLI**
- Clé voix invalide (ou `Welcome` absent) **fige** la CLI (attente jamais résolue) — [`client-cli/src/main.rs:96`](crates/client-cli/src/main.rs:96).
- Pas de fermeture propre → fantôme dans la liste ~30 s ; `/mic on` et `/lock` sans mot de passe confirment un faux succès — [`client-cli/src/main.rs:440`](crates/client-cli/src/main.rs:440).

---

## Ce qui est sain (vérifié, pour ne pas y revenir)

- Argon2id avec sel aléatoire par compte, vérification par la bibliothèque ; `ChangePassword` vérifie bien l'ancien.
- Pas de course sur les invitations (verrou tenu sur tout `authenticate`) ; TTL et révocation respectés.
- Rôles système non supprimables/renommables, `ROLE_OWNER` impossible à retirer au propriétaire (par le rang).
- `AdminReorderChannels` exige une permutation exacte (doublons compris) ; `next_id` des salons monotone même après perte de `channels.json`.
- Traversée de chemin fermée dans `files.rs` (id hex 16 car., `..`/`/`/`\` neutralisés) ; pas d'injection JSONL dans l'audit.
- `check_png`/`check_thumbnail` : bombe de décompression et fichiers polyglottes refusés sans décodage.
- `secret_eq` à temps constant ; `MAX_LINE` **appliqué en lecture** des deux côtés ; tous les `match` de messages exhaustifs (survie aux variantes inconnues).
- `secret.rs` : DPAPI en portée utilisateur, refus de repli en clair ; rééchantillonneur cubique correct (dérive/coutures maîtrisées).

---

## Priorisation suggérée

1. **Empêcher le serveur de mourir / se figer :** C1, C2, C4, C5 (+ M27, M28, M31, M24).
   Ce sont les bugs « le serveur tombe et ne revient pas ».
2. **Rendre le chat de nouveau fiable :** C3, M7, M8, M9 — la boucle History/pagination et le salon-piège.
3. **Faire fonctionner la modération pour de vrai :** M1, M2, M3, M4, M5, M6, M25.
4. **Réparer le vocal :** C6, M11, M12, M13, M14, M16, M17, M18.
5. **Durcir le transport :** M22, M23 (TOFU), M15 (nonce).
6. Le reste (UI, CLI, latents) au fil de l'eau.
