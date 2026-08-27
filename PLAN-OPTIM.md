# Audit 0.1.11 et plan de développement

**Date :** 2026-08-27 · **Version auditée :** 0.1.11 (`1626ed3`)
**Périmètre :** les 8 crates, 23 087 lignes de Rust, la chaîne de compilation
et les deux workflows d'intégration continue.

**Méthode :** lecture du code — pas de la documentation — sur les chemins qui
comptent (relais vocal, rappels audio temps réel, boucle de rendu, diffusion
serveur, pagination de l'historique), plus l'exécution réelle de la chaîne de
vérification. `cargo test --workspace` : **vert**. `cargo clippy --workspace
--all-targets` : **0 erreur, 17 avertissements** (tous cosmétiques —
`sort_by_key`, `mut` inutile, deux fonctions mortes).

**Deux consignes reçues, qui gouvernent tout ce qui suit :**

1. **On reste en Rust.** Aucune proposition ci-dessous n'introduit un runtime
   étranger, un moteur d'interface web ou un langage de script. Les deux
   bibliothèques C déjà présentes (libopus, openh264) sont compilées depuis
   leurs sources par nos propres crates de build, et ne se remplacent pas
   aujourd'hui sans perdre DRED et le Deep PLC.
2. **L'objectif est l'application la plus optimisée possible.** Ce n'est pas
   un vœu décoratif : il fixe l'ordre des travaux. Ce qui coûte du temps *à
   chaque image* ou *à chaque trame audio* passe avant ce qui coûte du temps
   une fois par jour.

---

## Résumé

Le socle est **bon** : la sécurité a été traitée sérieusement (Argon2id hors
verrou, TOFU sur le certificat, voix chiffrée de bout en bout, entrées
bornées), le protocole survit aux variantes inconnues, et 132 tests couvrent
le rééchantillonnage, le tampon de gigue, les rôles et les comptes.

Trois constats dominent l'audit :

- **Rien ne garde la porte.** Les 132 tests et le `clippy` propre ne sont
  exécutés par **aucun** workflow. On publie un binaire compilé, jamais un
  binaire testé. C'est le défaut le plus grave du projet, parce qu'il
  conditionne tous les autres : sans filet, chaque optimisation devient un
  pari.
- **Le client dépense en permanence ce qu'il ne consomme pas.** Vingt images
  par seconde inconditionnelles, le fil de discussion entièrement re-parcouru
  à chaque image, et des copies complètes de l'état (membres, rôles, salons,
  journal) à chaque image aussi. Sur un poste qui fait tourner Valorant à
  côté, c'est exactement le budget qu'on n'a pas.
- **Le chemin temps réel alloue.** Les deux rappels audio — capture et
  rendu — appellent l'allocateur à chaque bloc, et le rappel de sortie prend
  quatre à cinq verrous par trame de 20 ms. Ça marche aujourd'hui ; ça
  craquera le jour où la machine sera chargée, c'est-à-dire précisément quand
  on en a besoin.

Le reste de la dette (l'audit 0.1.3, dont les lots critiques, modération,
vocal et transport ont été soldés) tient en une douzaine de points vérifiés
encore ouverts, listés plus bas.

---

# Partie I — Audit

## 1. Ce qui est sain (vérifié aujourd'hui, pour ne pas y revenir)

- **Relais vocal serveur** : table de routage précalculée en `RwLock`, lue une
  fois par datagramme, `Bytes` cloné par compteur de références et non copié.
  C'est la bonne architecture — le serveur ne décode ni ne déchiffre rien.
- **Chaîne DSP de capture** : gain, débruitage, porte, AGC et vumètre opèrent
  tous **en place** sur un `[f32; FRAME_SAMPLES]`. Pas une allocation entre le
  rééchantillonneur et l'encodeur.
- **Bornes du protocole** : `MAX_LINE` appliqué en *lecture* des deux côtés,
  `VOICE_MAX_PACKET` vérifié côté serveur, en-tête PNG contrôlé sans décodage.
- **Ordonnancement des tâches serveur** : Argon2id, avatars, bannissements et
  invitations partent sur le pool bloquant.
- **Profil de compilation** : `lto = "fat"`, `codegen-units = 1`,
  `panic = "abort"`, `strip = true`. Et le profil `dev` qui optimise les
  dépendances (`opt-level = 3` sur `*`) est une trouvaille : sans lui la vidéo
  tombait à 2 fps en débogage.
- **Anti-force brute** : le refus tombe avant le hachage. C'est le point qui
  compte, et il est bien placé.

## 2. Le trou de process : la chaîne de publication ne vérifie rien

`release.yml` compile `ki-client-gui`, vérifie l'autonomie du binaire par
`dumpbin`, fabrique l'installeur et publie. `docker.yml` construit l'image du
serveur. **Ni l'un ni l'autre n'exécute `cargo test` ou `cargo clippy`.**

C'est vérifiable en une commande : `grep -rn "cargo test\|clippy" .github/` ne
renvoie rien.

Conséquence concrète : une régression dans le tampon de gigue, dans les rôles
ou dans le rééchantillonneur part en release sans qu'aucun signal ne se lève.
Les tests existent, ils sont bons, ils sont écrits — et ils ne tournent que
quand quelqu'un y pense.

Ce défaut est **le premier à corriger**, avant toute optimisation, parce que
l'optimisation consiste précisément à réécrire du code qui marche.

## 3. Dette de l'audit 0.1.3, vérifiée encore ouverte

Les lots critiques (C1–C6), modération (M1–M6), vocal (M11–M18) et transport
(M22–M23) ont bien été soldés. Ce qui suit a été **relu dans le code
d'aujourd'hui** et reste ouvert.

| # | Point | Emplacement | Pourquoi ça compte |
|---|---|---|---|
| M19 | « Connexion… » sans délai ni annulation | `client-gui/src/main.rs:1135` | Le keepalive de 5 s empêche l'expiration d'inactivité de jouer : l'écran peut rester bloqué indéfiniment |
| M21 | `disconnect()` ne réinitialise pas tout l'état | `client-gui/src/main.rs` | Les `user_id` étant par-serveur, on voit la photo de quelqu'un d'autre après un changement de serveur |
| M24 | `accept_bi()` sans délai | `server/src/quic.rs:124` | Le délai de 10 s ne couvre que la première *ligne*, pas l'ouverture du *flux* : des connexions non authentifiées se garent indéfiniment |
| M26 | `ChangePassword` hache avant de vérifier | `server/src/accounts.rs:504` | `hash_password(new)` précède la vérification de l'ancien : un Argon2id complet par tentative fausse. Le seau à jetons (100/s) borne l'amplification sans la supprimer |
| M27 | `save()` avale les échecs disque | `accounts.rs:661`, `roles.rs`, `channels.rs` | Disque plein : l'interface confirme un bannissement qui disparaîtra au redémarrage |
| M28 | Écritures encore sur la boucle asynchrone | `server/src/quic.rs:1134-1394` | Les chemins avatar / ban / invitation sont passés sur le pool bloquant ; **rôles et salons ne l'ont pas été**, et les 16 `audit.record` sont des `writeln!` synchrones |
| M29 | `ServerMeta::update` écrit hors du verrou | `server/src/meta.rs:44` | Deux admins simultanés : mise à jour perdue, ou `server.tmp` entrelacé |
| M31 | `audit.jsonl` pivoté au seul démarrage | `server/src/audit.rs:42` | `rotate_if_large` n'est appelé qu'à l'ouverture : un serveur qui ne redémarre pas laisse le journal grandir sans borne |
| M32 | Aperçus d'images : éviction des entrées `Loading` | `client-gui/src/images.rs:150` | Évincer une entrée en cours de chargement relance un `thread::spawn` ; à 20 images/s et 41 images dans le fil, c'est une tempête de fils |
| M33 | Images décodées sur le fil de l'interface | `client-gui/src/images.rs:118` | `mount()` appelle `decode()` : jusqu'à 8000 px et 64 Mio d'allocation, en pleine boucle de rendu |
| M34 | Texture d'aperçu partagée logo / avatar | `client-gui/src/main.rs` | Re-décodage PNG + téléversement GPU à chaque image quand les deux panneaux sont ouverts |

S'y ajoutent les mineurs déjà listés dans `AUDIT.md` (saisie mono-ligne,
troncature des pseudos longs, `safe_display` manquant sur le chat en direct,
pseudos non filtrés des commandes bidirectionnelles côté serveur, `next_id`
non reconsolidé au démarrage, `before()` qui tronque à la première ligne
illisible). Aucun n'est urgent ; tous sont réels.

## 4. Performance du client — le poste le plus chargé

C'est ici que se joue la consigne. Le serveur tourne sur un VPS qui ne fait
que ça ; le client tourne à côté d'un jeu.

### 4.1 Vingt images par seconde, quoi qu'il arrive

`main.rs:5867` — la dernière ligne de `update()` :

```rust
ctx.request_repaint_after(std::time::Duration::from_millis(50));
```

Inconditionnel. À l'écran de connexion, fenêtre réduite, application en
arrière-plan pendant une partie : vingt reconstructions complètes de l'arbre
de widgets par seconde, vingt mises en page, vingt téléversements de maillage
vers le GPU.

La justification écrite est valable — le push-to-talk global est échantillonné
dans la boucle de rendu — mais elle prouve l'inverse de ce qu'elle veut :
**c'est le sondage clavier qui doit sortir de la boucle de rendu**, pas le
rendu qui doit s'aligner sur lui. Un fil dédié à 100 Hz coûte une fraction de
pour-cent et rend la touche *plus* réactive (l'audit précédent note déjà que
20 Hz rate les pressions brèves).

### 4.2 Le fil de discussion n'est pas virtualisé

`main.rs:3130-3163` — `chat_log` parcourt **tous** les messages en mémoire
(jusqu'à 500, davantage après pagination) à chaque image, qu'ils soient
visibles ou non. Et pour chacun :

```rust
let color = self.members.iter().find(|m| m.user_id == msg.user_id)...
```

Une recherche **linéaire** dans la liste des membres. Avec 500 messages et
30 membres, c'est 15 000 comparaisons par image, soit 300 000 par seconde,
pour un résultat qui ne change jamais entre deux images.

`egui::ScrollArea` fournit `show_rows` / `show_viewport` exactement pour ce
cas. Et la couleur d'un auteur se met en cache dans une `HashMap<UserId,
Color32>` invalidée à la réception d'un `Members`.

### 4.3 Des copies complètes de l'état à chaque image

Vingt-sept `clone()` dans le chemin de rendu, dont ceux-ci, sur des structures
entières :

```
main.rs:2887   let members = self.members.clone();   // Vec<Member> : String + Vec<RoleId> + empreinte, par membre
main.rs:2692   let channels = self.channels.clone();
main.rs:4390   let roles = self.roles.clone();
main.rs:4745   let users = self.admin_users.clone();
main.rs:4680   let invites = self.admin_invites.clone();
main.rs:4974   let records = self.audit.clone();
```

Ce sont des contournements du vérificateur d'emprunts : on copie parce qu'on
veut lire `self.members` tout en appelant `self.member_menu(&mut self)`. Le
code le sait déjà, puisque `chat_log` emploie la bonne technique
(`std::mem::take` puis restitution) pour les messages.

La correction propre n'est pas de multiplier les `mem::take` : c'est de
**séparer l'état de données de l'état d'interface** en deux structures, ce qui
rend les emprunts disjoints et supprime la question. À 20 images par seconde,
on parle de plusieurs mégaoctets par seconde d'allocations qui ne servent à
rien.

### 4.4 Le décodage d'image sur le fil de rendu

`images.rs:118` — `mount()` appelle `decode()`, borné à 8000 px et 64 Mio
d'allocation, dans la boucle de rendu. Une photo un peu grande fige la
fenêtre. Le fil de téléchargement existe déjà et ne fait rien pendant ce
temps : c'est là que le décodage doit avoir lieu, et `mount()` ne doit plus
recevoir que des `ColorImage` prêtes à téléverser.

## 5. Performance du moteur audio — le chemin temps réel

Un rappel audio n'a le droit ni d'allouer, ni de prendre un verrou qui peut
être tenu par un fil de priorité normale. La première règle protège du
`GlobalAlloc` de Windows, qui prend un verrou de tas ; la seconde de
l'inversion de priorité.

### 5.1 Deux allocations par bloc capturé

`lib.rs:1704` — dans le rappel du micro :

```rust
let f: Vec<f32> = data.iter().map(|s| s.to_sample::<f32>()).collect();
tx.send(to_mono(&f, channels)).ok();
```

`collect()` alloue, `to_mono()` alloue une seconde fois. Le moteur natif
WASAPI a le même schéma (`fold_mono_f32`, `wasapi.rs:891`).

### 5.2 Une allocation par bloc rendu, et cinq verrous par trame

`lib.rs:1938` — dans le rappel de sortie :

```rust
let mut out = vec![0f32; mono_needed];   // allocation, à chaque appel
```

Et pour chaque trame de 20 ms produite, dans le même rappel : `volumes.lock()`,
`playouts.lock()`, `playout.lock()` **par locuteur**, `loopback_buf.lock()`,
`effects_buf.lock()`. Le commentaire prend soin de noter que le décodage ne se
fait pas sous ces verrous — c'est vrai, et c'est la partie difficile, déjà
réglée. Il reste que `recv_loop` écrit dans les mêmes `Mutex` depuis un fil
ordinaire.

La forme correcte est connue : un tampon circulaire sans verrou (`ringbuf`,
100 % Rust) par locuteur, une liste de locuteurs publiée par échange atomique
de pointeur, et un `Vec` de sortie **préalloué une fois** dans la fermeture au
lieu d'être créé à chaque appel.

### 5.3 Canaux non bornés sur le chemin de la voix

`net.rs:319` : `tx.send(dat.to_vec())` — copie du datagramme (alors que
`Bytes` est déjà compté par références), poussée dans un `std::sync::mpsc`
**non borné**. Si `recv_loop` prend du retard, la file grandit sans limite. Un
canal borné qui **jette la plus ancienne** est le bon comportement pour de la
voix : une trame en retard ne vaut rien.

### 5.4 Le mixage ne se vectorise pas

`jitter.rs:117` — `Playout::mix_into` fait `self.ready.pop_front().unwrap()`
échantillon par échantillon sur un `VecDeque<f32>`. Le compilateur ne peut pas
vectoriser : à chaque itération il y a une décrémentation de longueur, un
calcul d'indice modulaire et une vérification de bornes.

Un tampon circulaire exposant deux tranches contiguës laisse LLVM produire du
SSE/AVX sur la boucle de mixage. Avec dix locuteurs simultanés, cette boucle
est le seul endroit du client où il y a réellement du calcul par échantillon.

## 6. Performance du serveur

### 6.1 Un message diffusé est sérialisé une fois par destinataire

`state.rs:480` :

```rust
pub fn broadcast_all(&self, msg: &ServerMsg) {
    let users = self.users.lock().unwrap();
    for u in users.values() {
        let _ = u.tx.send(msg.clone());     // clone profond par destinataire
    }
}
```

Puis chaque tâche d'écriture fait son propre `serde_json::to_string(&msg)`.

Pour un `Members` à 30 membres — émis à chaque connexion, déconnexion, entrée
et sortie de vocal — cela fait 30 copies profondes (chacune avec 30 `String`,
30 `Vec<RoleId>`, 30 empreintes d'avatar) **et** 30 sérialisations JSON du
même contenu : une centaine de kilo-octets produits pour envoyer quatre
kilo-octets trente fois.

La correction est mécanique et sans risque : sérialiser **une fois** en
`Arc<[u8]>`, et faire porter au canal des octets prêts à écrire au lieu d'un
`ServerMsg`. Elle supprime au passage le garde-fou `MAX_LINE` dupliqué dans
chaque tâche d'écriture.

### 6.2 Remonter le fil relit tout le fichier

`history.rs:279` — `before()` relit **l'intégralité** de `channel-N.jsonl`,
désérialise chaque ligne, filtre, trie, et jette tout sauf une page de
cinquante messages. Le code l'assume dans son commentaire :

> Un salon de 100 000 messages fait ~15 Mo — coûteux mais rare, et sans index
> il n'y a pas de moyen honnête de faire mieux. C'est le moment de passer à
> SQLite si les salons grossissent vraiment.

Ce n'est pas rare : remonter une conversation enchaîne les requêtes au rythme
des réponses. Dix pages, c'est dix relectures de 15 Mo et 10 × 100 000
désérialisations JSON, sur le pool bloquant d'un VPS à deux cœurs. Et ce même
défaut interdit la recherche dans l'historique, qui est la fonctionnalité la
plus demandée d'un chat.

### 6.3 Des écritures disque encore sur la boucle asynchrone

`handle_msg` est **synchrone** et appelée depuis la tâche asynchrone de
contrôle. Les chemins avatar, bannissement et invitation ont bien été passés
sur `spawn_blocking` ; **les rôles et les salons ne l'ont pas été**
(`quic.rs:1134`, `:1188`, `:1221`, `:1307`, `:1322`…), et les seize
`audit.record` sont des `writeln!` synchrones sous mutex (`audit.rs:76`).

Chaque création de rôle ou de salon réécrit donc un fichier JSON complet et
bloque un ouvrier tokio — le même qui porte des tâches de relais vocal.

## 7. Compilation, binaire, allocateur

- **`target-cpu` par défaut** : le binaire est compilé pour le x86-64 d'origine
  (1999). Aucun SSE4, aucun AVX. Tout le DSP — rééchantillonneur cubique,
  mixage, DeepFilterNet via `tract` — s'exécute en scalaire. Passer à
  `x86-64-v2` (SSE4.2, POPCNT — universel depuis 2009) est gratuit et sans
  risque ; `v3` (AVX2, Haswell 2013) est un gain plus net mais exclut de
  vieilles machines, donc à décider explicitement.
- **Allocateur** : sous Windows, Rust utilise le tas système, qui prend un
  verrou global. C'est exactement le mauvais allocateur pour une application
  qui alloue en rafale depuis plusieurs fils. La bonne réponse ici est d'abord
  d'**arrêter d'allouer** dans les chemins chauds (points 4 et 5) — c'est du
  Rust pur et ça règle la cause, pas le symptôme.
- **Optimisation guidée par le profil (PGO)** : la chaîne existe dans `rustc`
  et ne demande aucun changement de code, seulement deux passes dans le
  workflow. Sur une application dominée par des branches (parseur JSON, arbre
  de widgets), c'est typiquement 5 à 15 % — mesurables.
- **Taille** : 31 Mo pour `ki-chat.exe`. Ce n'est pas un problème de
  performance, mais c'est 31 Mo téléchargés à chaque mise à jour automatique
  par trente personnes. Une mise à jour **différentielle** est la vraie
  réponse ; la réduction de taille en est une pauvre.
- **Mise à jour automatique sans signature** : `update.rs:236` — le
  téléchargement n'a **pas de délai d'expiration** et n'est vérifié que par sa
  *taille*. La seule garantie d'intégrité est TLS jusqu'à GitHub. Une
  application qui remplace son propre exécutable devrait vérifier une signature
  Ed25519 dont la clé publique est gravée dans le binaire. C'est le point de
  sécurité le plus rentable qui reste.

---

# Partie II — Plan de développement

Les phases sont ordonnées pour que chacune rende la suivante plus sûre. On ne
commence pas par optimiser : on commence par pouvoir constater qu'on n'a rien
cassé, puis par pouvoir mesurer ce qu'on gagne.

## P0 — Poser le filet ✅ (livré le 2026-08-27)

**Sans ça, aucune optimisation n'est défendable.**

1. ✅ **Workflow `ci.yml`** sur `push`, `pull_request` et `workflow_call` :
   `cargo clippy --workspace --all-targets --locked -- -D warnings` puis
   `cargo test --workspace --locked` sur `windows-latest` — le seul système où
   tout compile, WASAPI et DPAPI étant derrière un `#[cfg(windows)]` — plus une
   passe `ubuntu-latest` restreinte à `ki-server` et `ki-protocol`, là où le
   serveur tourne vraiment.
2. ✅ **`release.yml` et `docker.yml` dépendent de `ci.yml`.** L'image du
   serveur est gatée elle aussi : elle se déploie toute seule (Watchtower sonde
   toutes les cinq minutes), donc une régression y serait en production avant
   d'être constatée. Elle appelle `ci.yml` avec `only_server: true` pour ne pas
   attendre derrière la compilation d'openh264 et d'une interface graphique
   dont elle ne dépend pas.
3. ✅ **Les 17 avertissements de clippy sont soldés**, `-D warnings` posé.
   `require_admin` était bien mort et a été supprimé ; `exists` ne l'était pas —
   un test l'utilisait, et il dit désormais la même chose avec `get()`, qui est
   de l'API vivante. Une correction dépasse le lint : `roster()` trie
   maintenant par `sort_by_cached_key`, là où `sort_by_key` aurait réalloué la
   clé (une `String` minuscule) à chaque comparaison — sur une liste rediffusée
   à chaque entrée et sortie de vocal.

> **`cargo fmt --check` n'est volontairement pas dans le filet.** La base n'est
> pas au format rustfmt : 419 hunks de différence, et la meilleure configuration
> trouvée (`max_width = 96`, `use_small_heuristics = "Max"`) en laisse encore
> 242. Le style manuel est délibéré et souvent meilleur que le défaut — il garde
> `ServerMsg::VoiceState { user_id, speaking, muted } =>` sur une ligne là où
> rustfmt l'éclate en six. Adopter rustfmt reste possible, mais c'est une
> décision à prendre pour elle-même, dans son propre commit, avec un
> `.git-blame-ignore-revs` — pas un effet de bord de l'introduction de la CI, où
> 6 000 lignes de reformatage enterreraient le reste.

**Vérifié sur la machine** : `clippy -D warnings` sort à 0 sur tout l'espace de
travail, `cargo test --workspace --locked` passe 132 tests, et les trois
workflows sont syntaxiquement valides.

*Coût réel : une session. Gain : tout le reste devient possible.*

## P1 — Pouvoir mesurer

Optimiser sans mesure produit du code plus compliqué et pas plus rapide.

1. **`criterion` sur les trois chemins qui comptent** : mixage de N locuteurs
   (`Playout::mix_into`), rééchantillonnage cubique, sérialisation d'un
   `Members` à 30 membres. Ce sont les repères contre lesquels P3 et P4 se
   jugeront.
2. **Compteurs de temps d'image dans le client** : temps de `update()`, temps
   de mise en page du fil, nombre d'allocations par image. Affichés dans le
   panneau de diagnostic existant (celui du journal audio), pas dans
   l'interface principale.
3. **Compteur de sous-alimentations du rappel de sortie** (`underruns`) dans
   les statistiques voix. Aujourd'hui, un craquement ne laisse aucune trace :
   on ne peut ni le reproduire, ni prouver qu'il a disparu.
4. **Une charge de test** : un binaire `ki-load` qui ouvre N connexions QUIC
   authentifiées et émet de la voix. Trente personnes ne se réunissent pas sur
   commande pour valider un correctif.

*Coût : une à deux sessions.*

## P2 — Solder la dette vérifiée

Dans cet ordre, parce qu'il va du « ça casse le service » au « ça agace ».

1. **M24** — délai autour de `accept_bi`, plafond de connexions par IP.
2. **M26** — vérifier l'ancien mot de passe **avant** de hacher le nouveau.
3. **M28** — `spawn_blocking` sur les chemins rôles et salons ; `audit.record`
   sur un fil d'écriture dédié avec canal (le même patron que `History`).
4. **M27** — propager les échecs de `save()` jusqu'au client. Une modération
   qui échoue doit se voir.
5. **M31** — vérifier la taille du journal d'audit **à l'écriture**, purger les
   archives.
6. **M29** — écrire `server.json` sous le verrou, fichier temporaire unique.
7. **M19 / M21** — délai et annulation sur « Connexion… » ; réinitialisation
   exhaustive à la déconnexion (ou reconstruction de `KiApp`, plus sûr).
8. **M32 / M33 / M34** — ne jamais évincer une entrée `Loading`, décoder dans
   le fil de téléchargement, deux entrées de cache distinctes.

*Coût : deux à trois sessions.*

## P3 — Le client : ne plus dépenser ce qu'on ne consomme pas

C'est la phase au plus fort rendement pour la consigne d'optimisation, parce
que c'est le seul processus qui partage sa machine avec un jeu.

1. **Sortir le sondage du push-to-talk de la boucle de rendu.** Un fil dédié à
   100 Hz, qui ne réveille l'interface que sur changement d'état. La touche
   devient plus fiable au passage.
2. **Repeint conditionnel.** `request_repaint_after` seulement quand quelque
   chose bouge réellement : quelqu'un parle, un vumètre est visible, une
   animation est en cours. Au repos, l'application ne doit pas repeindre du
   tout — egui se réveille sur les événements. *Cible : moins de 1 % de CPU
   fenêtre réduite, contre plusieurs pour-cent aujourd'hui.*
3. **Virtualiser le fil de discussion** (`show_rows`) et **mettre en cache la
   couleur d'auteur** dans une `HashMap` invalidée à la réception d'un
   `Members`. *Cible : temps de rendu du fil indépendant du nombre de messages
   chargés.*
4. **Séparer l'état de données de l'état d'interface** pour supprimer les
   copies complètes par image. C'est une refonte de `main.rs`, qui à 5 981
   lignes en a de toute façon besoin — le découper en modules (`chat.rs`,
   `admin.rs`, `audio_panel.rs`, `roster.rs`) est la moitié du travail.
5. **Cache de textures et d'aperçus** : compte correct des entrées, éviction
   qui n'atteint jamais un chargement en vol.

*Coût : trois à quatre sessions. C'est la phase la plus longue et la plus
rentable.*

## P4 — Le moteur audio : un chemin temps réel qui mérite son nom

1. **Zéro allocation dans les deux rappels.** Tampons préalloués tenus dans la
   fermeture ; conversion et repli mono en place vers une tranche fournie.
2. **Tampons circulaires sans verrou** (`ringbuf`, 100 % Rust) entre
   `recv_loop` et le rappel de sortie, et entre le rappel de capture et
   `capture_loop`. Le rappel ne prend plus aucun `Mutex`.
3. **Liste de locuteurs publiée par échange atomique** (`arc-swap`) : le rappel
   de sortie lit un instantané sans verrou ; `recv_loop` publie une nouvelle
   liste à l'apparition ou au départ d'un locuteur.
4. **Mixage vectorisable** : tampon circulaire à tranches contiguës, boucle sur
   `&[f32]` au lieu de `pop_front()`. Vérifié par le banc de P1.
5. **Canaux bornés à politique d'éviction** sur le chemin de la voix, et
   `Bytes` transmis tel quel au lieu de `to_vec()`.
6. **Priorité temps réel du fil audio** (`AvSetMmThreadCharacteristics`, classe
   « Pro Audio ») — ce que fait tout moteur audio Windows sérieux, et ce qui
   protège des craquements quand un jeu sature les cœurs.

*Cible mesurable : zéro sous-alimentation du rappel de sortie pendant une
partie de Valorant, avec dix locuteurs simultanés et DeepFilterNet actif.*

*Coût : deux à trois sessions.*

## P5 — Le serveur : payer une fois ce qu'on paie N fois

1. **Sérialiser une fois par message, pas une fois par destinataire.** Le canal
   porte des `Arc<[u8]>` prêts à écrire. *Gain : d'un facteur N sur le coût de
   diffusion, avec N = nombre de connectés.*
2. **Indexer l'historique.** Deux options honnêtes :
   - **Index d'offsets** en mémoire (`Vec<(ts, offset)>` par salon, construit au
     démarrage, maintenu à l'écriture) : `before()` devient une recherche
     binaire plus un `seek`. Reste du Rust pur, aucune dépendance, aucun
     changement de format sur disque, aucune migration.
   - **SQLite** (`rusqlite`, libsqlite3 embarquée) : plus de travail, mais ouvre
     la **recherche plein texte** et les réactions, éditions et réponses sans
     réinventer un moteur.

   *Recommandation : l'index d'offsets d'abord (une session, gain immédiat,
   zéro risque), SQLite quand la recherche deviendra une fonctionnalité
   décidée.*
3. **Diffusion différentielle du roster** : `MemberUpdate { user_id, … }` au
   lieu d'un `Members` complet à chaque entrée et sortie de vocal. Le protocole
   gère déjà les variantes inconnues, donc l'ajout est compatible.

*Coût : deux sessions.*

## P6 — Compilation et livraison

1. **`target-cpu=x86-64-v2`** dans `.cargo/config.toml`, validé par le banc de
   P1. Décider explicitement pour `v3` (AVX2) — le gain sur le DSP est réel,
   l'exclusion des machines d'avant 2013 aussi.
2. **PGO en deux passes dans `release.yml`** : compilation instrumentée,
   exécution de la charge de P1, recompilation avec le profil. Aucun changement
   de code.
3. **Signature Ed25519 des releases** (`ed25519-dalek`, 100 % Rust), clé
   publique gravée dans le binaire, vérification avant `install()`. Plus un
   délai d'expiration sur le téléchargement.
4. **Mise à jour différentielle** (`bidiff` / `zstd`, tous deux en Rust) :
   quelques centaines de kilo-octets au lieu de 31 Mo.

*Coût : une à deux sessions.*

---

# Partie III — Fonctionnalités

Une fois le socle rapide et gardé, dans l'ordre de la valeur rendue.

## F1 — Palier 3 du chantier audio : le « docteur audio »

Le seul palier non commencé du chantier ouvert en août. Détecter au démarrage
les suites logicielles qui s'interposent (Sonar, Synapse, G Hub, Nahimic,
NVIDIA Broadcast, Voicemeeter), lire l'état « mode exclusif autorisé » de
l'endpoint, et donner un conseil ciblé — désactiver la voix Vivox intégrée de
Valorant, passer le jeu en fenêtré sans bordure.

Pas d'écriture dans le registre : conseiller, jamais agir à la place de
l'utilisateur.

*C'est la fonctionnalité qui règle le problème le plus concret et le plus
ancien des trente utilisateurs.*

## F2 — Le confort de chat qui manque

Chacun de ces points est petit ; ensemble ils font la différence entre « ça
marche » et « on l'utilise ».

- **Saisie multi-ligne** (Maj+Entrée) — le protocole la gère déjà, seule
  l'interface l'interdit.
- **Mentions `@pseudo`** avec surlignage et notification.
- **Réponses** (citer un message) et **édition / suppression** de ses propres
  messages.
- **Recherche dans l'historique** — dépend directement de P5.2.
- **Rendu Markdown léger** (gras, italique, code, blocs) et émojis.
- **Renommer / réordonner un salon, poser un mot de passe vocal** depuis
  l'interface : ces trois actions n'existent aujourd'hui qu'en CLI.

## F3 — Modération vocale

Couper le micro de quelqu'un côté serveur, le rendre sourd, le déplacer de
salon vocal. Ce sont les trois gestes qu'un modérateur attend et qui n'existent
pas. Les permissions et les rangs sont déjà en place : c'est du protocole et de
l'interface, pas de l'architecture.

## F4 — Partage d'écran

`PLAN-STREAM.md` est un plan sérieux, déjà validé par une revue adversariale,
avec S0, S0.5 et S1a livrés et mesurés (1080p, 26,3 fps, 14,4 ms par trame). Il
reprend à **S1b** et n'a pas besoin d'être refait.

Une seule remarque de cet audit : S1b suppose un client capable de dépasser
20 fps par `request_repaint()`. **P3.2 est donc un prérequis** — et il le rend
gratuit, puisqu'un repeint conditionnel monte naturellement à la fréquence de
l'écran quand il y a quelque chose à montrer.

## F5 — Overlay en jeu

Le point le plus ambitieux de la liste M9. Une fenêtre superposée à laquelle on
ne peut pas donner le focus (`WS_EX_LAYERED | WS_EX_TRANSPARENT |
WS_EX_NOACTIVATE`), montrant qui parle. Faisable en Rust avec une seconde
fenêtre `eframe` — ce qui la rend gratuite en dépendances, et directement
dépendante de P3.4 (l'état partagé doit être extractible de la fenêtre
principale).

À ne pas commencer avant que P3 et F4 soient finis.

## F6 — Portabilité

`secret.rs` a déjà été écrit pour ça (« brancher le Trousseau macOS sera un
`#[cfg]` de plus, pas une refonte »). Le vrai travail est ailleurs : le moteur
WASAPI natif est Windows-seul, mais le repli cpal existe et couvre Linux et
macOS. Un client Linux est atteignable en une ou deux sessions ; il n'a
d'intérêt que si quelqu'un le réclame.

---

# Tableau de bord

Ce qu'on doit pouvoir mesurer à la fin, et l'état de départ.

| Grandeur | Aujourd'hui | Cible |
|---|---|---|
| CPU client, fenêtre réduite, hors vocal | 20 images/s inconditionnelles | < 1 % |
| Temps de rendu du fil, 500 messages | proportionnel au nombre de messages | constant (virtualisé) |
| Allocations par image de rendu | plusieurs milliers (copies d'état) | ~ 0 en régime |
| Allocations par rappel audio | 1 (sortie), 2 (capture) | 0 |
| Verrous pris par le rappel de sortie | 4 à 5 par trame | 0 |
| Sous-alimentations pendant une partie | non mesuré | 0, et mesuré |
| Diffusion d'un `Members` (30 connectés) | 30 copies + 30 sérialisations | 1 sérialisation |
| Page d'historique (salon de 100 k messages) | relecture de ~15 Mo | recherche binaire + `seek` |
| Tests exécutés avant publication | 0 | 132 |
| Poids d'une mise à jour | 31 Mo | quelques centaines de Ko |

---

# Ce qu'on écarte, et pourquoi

- **Réécrire le protocole de contrôle en binaire** (bincode, protobuf,
  MessagePack). Le JSON ligne à ligne coûte peu à trente personnes, il se lit au
  `grep` sur le serveur, et il survit aux variantes inconnues — trois propriétés
  qui valent plus que les kilo-octets gagnés. **Le vrai coût n'est pas le
  format, c'est de sérialiser N fois (P5.1).**
- **Remplacer egui.** Le grief n'est pas la bibliothèque, c'est l'usage qu'on en
  fait : repeint inconditionnel, absence de virtualisation, copies d'état.
  Changer de moteur d'interface ferait tout perdre pour ne rien régler — et les
  alternatives Rust matures pour ce type d'application sont rares.
- **Remplacer libopus et openh264 par des implémentations Rust pures.** Aucune
  ne fournit DRED ni le Deep PLC, et l'encodage H.264 en Rust pur n'existe pas à
  ce niveau de maturité. Ces deux bibliothèques sont compilées depuis leurs
  sources par nos propres crates de build, empreinte vérifiée : la contrainte
  « on reste en Rust » est respectée là où elle a un sens.
- **Un allocateur tiers en première intention.** Il masquerait les allocations
  des chemins chauds au lieu de les supprimer. À reconsidérer **après** P3 et
  P4, sur mesure, pas avant.
- **Un multithread ambitieux côté serveur.** Trente personnes ne saturent pas
  deux cœurs. Le problème du serveur n'est pas le parallélisme, c'est le travail
  redondant (P5).
