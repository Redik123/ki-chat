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

> **Mesuré depuis** (P1) : 6,51 µs → 1,33 µs à 5 destinataires, **214,5 µs →
> 7,22 µs à 30** (29,7×). La prédiction « d'un facteur N » est exacte.

### 6.1 bis — Et le roster grandit avec les comptes, pas avec les connectés

Trouvé en passant `ki-load` deux fois de suite sur le même serveur : à
**nombre de connectés identique** (12), le roster diffusé est passé de
1 252 à 2 691 octets. Entre les deux, le magasin de comptes était monté de
12 à 24.

C'est voulu et documenté — `roster()` liste « toute la communauté, pas
seulement les présents », comptes hors ligne compris — mais la conséquence ne
l'était pas : **le coût d'une diffusion suit le nombre de comptes créés depuis
toujours**, pas le nombre de gens dans la pièce. Un serveur de trente
habitués qui a vu passer deux cents personnes en un an sérialise deux cents
membres, trente fois, à chaque entrée et sortie de vocal.

Deux corrections, complémentaires :

- **P5.1** divise par le nombre de destinataires (la sérialisation unique) ;
- **P5.3** divise par le nombre de membres (`MemberUpdate` différentiel au
  lieu du roster entier). Cette trouvaille la fait passer de confort à
  nécessaire — et elle porte aussi la pagination des comptes hors ligne, si
  le magasin devait vraiment grossir.

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

## P1 — Pouvoir mesurer ✅ (livré le 2026-08-27)

Optimiser sans mesure produit du code plus compliqué et pas plus rapide.

1. ✅ **`criterion` sur les trois chemins qui comptent.**
   `crates/voice/benches/moteur.rs` mesure le mixage de 1, 4 et 10 locuteurs
   (`Playout::mix_into`) et le rééchantillonnage cubique dans les deux sens
   plus l'identité ; `crates/protocol/benches/diffusion.rs` met face à face les
   deux façons de diffuser un roster. `jitter` et `resample` sont devenus
   publics pour ça : un banc criterion est un crate extérieur, il ne voit que
   l'API publique.

   **Le premier résultat solde une question de l'audit.** La diffusion d'un
   roster, mesurée :

   | Destinataires | Aujourd'hui (`msg.clone()` + JSON par client) | P5.1 (`Arc<[u8]>` sérialisé une fois) | Écart |
   |---|---|---|---|
   | 5  | 6,51 µs | 1,33 µs | **4,9×** |
   | 30 | 214,5 µs | 7,22 µs | **29,7×** |

   La prédiction « d'un facteur N » de l'audit est donc exacte, et 214 µs par
   diffusion sur un ouvrier tokio — à chaque connexion, déconnexion, entrée et
   sortie de vocal, sur un VPS à deux cœurs qui porte aussi le relais vocal —
   fait de P5.1 le meilleur rapport travail/gain du plan côté serveur.

   **Et le second résultat en réfute un morceau.** Le mixage, par trame de
   20 ms (960 échantillons) :

   | Locuteurs | Temps | Part du budget de 20 ms |
   |---|---|---|
   | 1  | 2,29 µs  | 0,011 % |
   | 4  | 9,28 µs  | 0,046 % |
   | 10 | 23,37 µs | **0,12 %** |

   Parfaitement linéaire, et **négligeable en valeur absolue**. À dix
   locuteurs, la boucle que l'audit désignait comme « le seul endroit du client
   où il y a réellement du calcul par échantillon » consomme un huit-centième
   du temps disponible. La vectoriser rendrait ~20 µs par trame, soit 0,1 % de
   processeur.

   Le rééchantillonnage dit la même chose : 5,68 µs (44,1 → 48), 4,86 µs
   (48 → 44,1), et — curiosité — **5,29 µs à l'identité 48 → 48**, c'est-à-dire
   autant qu'une vraie conversion : il n'y a pas de chemin rapide pour le
   rapport 1,0, pourtant le cas courant. Cela reste 0,03 % du budget.

   **Ce que ça change au plan** : le chemin audio n'est pas *lent*, il est
   *fragile*. Les allocations et les verrous du rappel de sortie restent à
   supprimer — mais pour la raison qui compte vraiment, les à-coups et
   l'inversion de priorité, pas pour un débit qui n'a jamais manqué. P4.4 est
   déclassé en conséquence, et c'est le compteur de sous-alimentations, pas le
   banc, qui jugera P4.

2. ✅ **Compteurs de temps d'image dans le client** (`crates/client-gui/src/perf.rs`),
   dans ⚙ → **Relevé de performance**, à côté du journal audio et copiable
   comme lui : temps de l'image complète, temps du fil de discussion, messages
   parcourus **sur** messages chargés (l'écart prouvera P3.3), et images par
   seconde **réellement peintes**.

   Des **quantiles** (p50 / p95 / max sur 600 images), pas des moyennes : une
   moyenne de 3 ms cache une image sur vingt à 40 ms, et c'est celle-là qui se
   voit. Un test le vérifie.

   Le compteur d'allocations par image existe aussi, mais derrière
   `--features mesures`, et ce n'est pas de la timidité : une incrémentation
   atomique par allocation, sur une ligne de cache que se disputent le fil de
   l'interface, celui du réseau et **celui de l'audio**, coûte précisément là
   où il ne faut pas. `cargo run -p ki-client-gui --features mesures` quand on
   veut le chiffre.

3. ✅ **Compteur de sous-alimentations.** `Playout` compte les trames qu'il n'a
   pas pu remplir — à sec (trou franc) comme partiellement — et le rappel de
   sortie les relève **sous le verrou qu'il tient déjà pour mixer**, sans
   second verrouillage sur le chemin temps réel. Une seule écriture atomique
   par trame, et seulement s'il y a eu un trou : le chemin propre ne paie rien.
   Remonte dans `VoiceStats::underruns` et dans le relevé.

4. ✅ **Charge de test** : `crates/load`, binaire `ki-load`. N vraies connexions
   QUIC, vrais comptes créés par code d'invitation, vraie voix chiffrée
   (XChaCha20-Poly1305, même dérivation de nonce que le moteur) à la taille et
   au rythme réels — 160 octets utiles, 50 trames par seconde.

   ```
   ki-load 127.0.0.1 --clients 30 --invite changeme --secondes 60 --muets 20
   ```

   Aucune dépendance au moteur audio : ni carte son, ni cpal, ni encodeur —
   trente encodeurs neuronaux mesureraient la machine, pas le serveur. La
   charge tourne donc aussi depuis un conteneur Linux posé à côté du serveur.
   `--muets` compte : un salon réel est surtout fait d'auditeurs, et ce sont
   eux qui font payer le relais.

   Le bilan sort l'**amplification** (reçus / émis, à comparer au nombre
   d'occupants), les **pertes montantes** que le serveur signale lui-même, et
   les **octets de contrôle reçus** — la grandeur exacte que P5.1 vise.

*Coût réel : une session.*

## P2 — Solder la dette vérifiée ✅ (livré le 2026-08-27)

Dans cet ordre, parce qu'il va du « ça casse le service » au « ça agace ».

1. ✅ **M24** — délai autour de `accept_bi` **et** plafond par adresse. Les
   deux, parce qu'ils ne couvrent pas la même chose : le délai de dix secondes
   ne portait que sur la première *ligne*, pas sur l'ouverture du *flux* qui la
   transporte, si bien qu'une connexion ne demandant jamais de flux
   bidirectionnel attendait indéfiniment — les keep-alives de QUIC tenant
   l'expiration d'inactivité en échec. Le `Sas` (`state.rs`) borne à 32 les
   connexions **non authentifiées** simultanées d'une même adresse ; la place
   est rendue par le `Drop` d'un jeton, donc sur tous les chemins de sortie de
   l'authentification, refus compris, et libérée dès l'entrée — trente joueurs
   derrière une même box ne se disputent rien.
2. ✅ **M26** — et c'étaient **deux** défauts, pas un. L'ordre d'abord : le
   nouveau mot de passe était haché *avant* que l'ancien soit vérifié, donc un
   Argon2id complet par tentative fausse. Mais la vérification elle-même
   tournait **sous le verrou des comptes** — exactement le défaut C4 soldé pour
   `authenticate` et resté ici : toute connexion, tout bannissement attendaient
   derrière. Les deux hachages sont maintenant hors verrou, et le compte est
   relu avant écriture (un admin a pu réinitialiser ce mot de passe pendant les
   centaines de millisecondes du hachage).
3. ✅ **M28** — `audit.record` part sur un fil d'écriture dédié, patron de
   `History` : la mémoire glissante — celle que lit le panneau
   d'administration — est à jour immédiatement, la ligne va au disque
   ailleurs. Les chemins rôles et salons remontent désormais leurs échecs
   (voir M27), ce qui les sort du même coup du « on écrit et on ne sait pas ».
4. ✅ **M27** — `save()` renvoie son verdict dans les trois magasins, et 21
   sites d'appel le propagent. Trois exceptions **assumées et commentées** :
   la levée d'un bannissement échu n'échoue pas la connexion (elle se
   reconstate à la tentative suivante, et refuser d'entrer parce que le disque
   est plein enfermerait dehors quelqu'un dont la sanction est terminée) ; les
   premières écritures au démarrage arrêtent le serveur ; le balayage d'un rôle
   supprimé signale sans défaire. `create_invite`, `remove_role` et
   `forget_role` ont changé de signature au passage — une invitation que le
   disque a refusée est un lien qui meurt au redémarrage.
5. ✅ **M31** — taille vérifiée **à l'écriture**, plus seulement à l'ouverture,
   et purge des archives au-delà de cinq. Un serveur qui ne redémarre pas — le
   but — ne passait jamais par la rotation.
6. ⚠️ **M29 — déjà corrigé, mon audit le listait à tort.** `ServerMeta::update`
   tient le verrou pendant l'écriture, et `write_atomic` porte un compteur de
   séquence qui rend chaque temporaire unique. Vérifié dans le code, et le
   commentaire de `meta.rs` explique déjà pourquoi.
7. ✅ **M19 / M21** — délai de vingt secondes sur « Connexion… », **et** le
   bouton devient une annulation pendant la tentative, avec le décompte : le
   délai seul laissait vingt secondes sans rien à cliquer après avoir tapé la
   mauvaise adresse. `disconnect()` efface maintenant tout ce qui venait du
   réseau — vignettes, panneau admin, journal, brouillons, empreinte — et rien
   d'autre : les réglages audio, le carnet et les volumes appartiennent à la
   machine, pas au serveur.
8. ✅ **M32 / M33 / M34** — une entrée `Loading` n'est plus jamais évincée (et
   quand tout est en chargement, on ne lance rien de plus : le nombre de fils
   en vol est borné par la taille du cache) ; le décodage PNG a quitté le fil
   de l'interface pour celui qui vient de télécharger ; une livraison orpheline
   — changement de serveur en cours de route — est jetée au lieu de créer une
   texture que rien n'évincera ; et les deux aperçus de vignette ont chacun
   leur créneau.

*Coût réel : une session.*

## P3 — Le client : ne plus dépenser ce qu'on ne consomme pas ✅ (livré le 2026-08-27)

C'est la phase au plus fort rendement pour la consigne d'optimisation, parce
que c'est le seul processus qui partage sa machine avec un jeu.

1. ✅ **Le push-to-talk a quitté la boucle de rendu.** `ptt::Watcher` sonde le
   clavier à **100 Hz** sur son fil, calcule le maintien après relâchement, et
   ne réveille la fenêtre qu'aux changements d'état. Hors mode push-to-talk il
   ne lit même pas le clavier.

   Le sondage à 20 Hz était doublement mauvais : une pression brève de moins de
   cinquante millisecondes passait entre deux images — sur un push-to-talk,
   rater une pression c'est rater une phrase — et cette contrainte imposait de
   repeindre en permanence. **La touche est donc à la fois plus fiable et moins
   chère**, ce qui est rare.

2. ✅ **Repeint conditionnel.** `repaint_delay()` ne demande une image de plus
   que si quelque chose **s'anime tout seul** : vumètres en vocal, réglages
   ouverts, envoi de fichier, téléchargement de mise à jour, périphérique audio
   en cours de réouverture, décompte du bouton d'annulation. Sinon, **rien** —
   l'application dort jusqu'au prochain événement.

   Ce qui vient de l'extérieur réveille déjà la fenêtre de lui-même : réseau,
   images téléchargées, sondes de serveurs, et maintenant la touche
   push-to-talk. C'est ce dernier point qui manquait pour que la boucle puisse
   s'arrêter.

   **Mesuré**, écran de connexion, vingt secondes d'observation, même binaire à
   une ligne près :

   | | Temps processeur | Charge |
   |---|---|---|
   | Repeint inconditionnel (avant) | 1,078 s / 20 s | **5,39 %** d'un cœur |
   | Repeint conditionnel (après) | 0,344 s / 20 s | **1,72 %** d'un cœur |

   Soit **3,1×**. Le reste n'est pas du gâchis : l'écran de connexion garde une
   image par seconde pour armer la sonde périodique des serveurs (voir
   ci-dessous), et egui a son propre travail d'entrées à faire.

   > **Un piège trouvé en vérifiant, et pas en écrivant.** La sonde des
   > serveurs est déclenchée *depuis le rendu*, toutes les vingt secondes. Sans
   > image, pas de déclenchement ; sans déclenchement, pas de résultat ; sans
   > résultat, pas de réveil — l'état des serveurs se figeait définitivement sur
   > ce qu'il était à l'ouverture. D'où l'image par seconde de l'écran de
   > connexion, qui reste un vingtième de l'ancien régime. Les trois autres
   > horloges du client ont été vérifiées à la main : l'expiration de
   > `voice_intent` et la calibration du micro sont pilotées par événement ou
   > par un panneau qui s'anime, donc intactes.

3. ✅ **Fil de discussion virtualisé, couleur d'auteur en cache.**

   `show_rows` ne convenait pas — il suppose des lignes de hauteur uniforme, et
   un message ne l'est pas (retour à la ligne, groupage, séparateurs de
   journée, aperçus d'images). La hauteur de chaque bloc est donc **mesurée à
   l'image où il est peint** et gardée ; à l'image suivante, un bloc hors écran
   réserve sa place sans rien construire. Le cache est vidé dès que la largeur
   change, et purgé de ce qui n'est plus affiché.

   Un piège s'est refermé en chemin et mérite d'être noté : mesurer avec
   `ui.cursor()` donne la hauteur **plus l'espacement** entre widgets, alors
   qu'`allocate_space` ajoute cet espacement par-dessus la hauteur demandée. Un
   espacement de trop par message sauté, et le fil s'allongeait à mesure qu'on
   le remontait. `ui.scope()` mesure exactement ce qu'`allocate_space`
   consommera.

   La couleur d'auteur se résout maintenant **une fois par roster** dans une
   `HashMap`, au lieu d'un `find` linéaire par message et par image : à 500
   messages et 30 membres, c'étaient 15 000 comparaisons par image, 300 000 par
   seconde, pour un résultat qui ne changeait qu'à la réception d'une liste.

4. ✅ **Copies d'état par image supprimées** sur les deux chemins chauds — la
   liste des membres et celle des salons passent par `std::mem::take` puis sont
   remises, la technique que le fil de discussion employait déjà. Les quatre
   autres (`roles`, `admin_users`, `admin_invites`, `audit`) ne vivent que dans
   le panneau d'administration, qui n'anime rien : depuis le point 2, il ne se
   repeint plus que sur événement, et le coût s'est effondré de lui-même.

   **La refonte de `main.rs` en modules n'a pas été faite** — et ne l'a pas
   été à dessein. Elle réglerait la cause (des emprunts disjoints rendraient
   les `take` inutiles) mais pas un symptôme de plus, alors qu'elle réécrirait
   6 000 lignes de rendu qu'aucun test ne couvre. Elle reste souhaitable ;
   c'est un chantier pour lui-même, pas une dépendance de l'optimisation.

5. ✅ **Cache de textures et d'aperçus** — fait en P2 (M32/M33/M34).

*Coût réel : une session.*

## P4 — Le moteur audio : un chemin temps réel qui mérite son nom ✅ (livré le 2026-08-27)

> **Révisé par les mesures de P1.** Ce chemin n'est pas *lent* — le mixage
> coûte 0,12 % du budget à dix locuteurs, le rééchantillonnage 0,03 %. Il est
> *fragile* : il alloue et il verrouille dans un rappel temps réel, ce qui
> produit des à-coups et de l'inversion de priorité, pas de la lenteur
> moyenne. Les points ci-dessous sont donc les mêmes, mais leur justification
> change — et leur juge n'est plus le banc, c'est le compteur de
> sous-alimentations.

**Ce qui a été fait :** 1, 5, 6 et 7 en entier ; 2 et 3 **non**, et le
paragraphe qui suit dit pourquoi.

> **Les tampons sans verrou (points 2 et 3) n'ont pas été écrits.** Ce n'est
> pas un oubli. Le décodage sous verrou — la vraie cause d'inversion de
> priorité — avait déjà été soldé (C6) : les verrous restants ne sont tenus
> que le temps de puiser des échantillons dans un tampon, quelques
> microsecondes. Les remplacer voudrait dire réécrire le cœur d'un moteur que
> trente personnes utilisent tous les jours, que je ne peux pas écouter d'ici,
> et dont l'historique récent est fait de régressions audio coûteuses. Le
> compteur de sous-alimentations est en place depuis P1 : **c'est lui qui doit
> dire si ça vaut la peine**, sur une vraie machine, pendant une vraie partie.
> Sans ce chiffre, ce serait exactement le pari que P1 existe pour interdire.

1. ✅ **Zéro allocation dans les deux rappels.**

   **Sortie** : le rappel rendait un `Vec` à chaque appel. Il écrit désormais
   dans un tampon fourni — `FnMut(&mut [f32])` au lieu de
   `FnMut(usize) -> Vec<f32>` — alloué une fois par l'appelant, des deux côtés
   (cpal et WASAPI natif).

   **Capture** : c'étaient **deux** allocations par bloc, la conversion en f32
   et le repli mono en produisant chacune une, cent fois par seconde. Une seule
   passe écrit maintenant dans un tampon **recyclé** : le consommateur le rend
   après usage, le rappel le reprend. C'est le patron du recyclage de trames de
   `ki-video`, appliqué à l'audio.

   Ce n'est pas le temps de l'allocation qui coûtait — c'est le verrou de tas
   de Windows, que se disputent l'interface, le réseau et la capture, et
   derrière lequel un fil temps réel n'a pas le droit d'attendre.

2. ❌ **Tampons circulaires sans verrou** — non fait. Voir l'encadré ci-dessus.
3. ❌ **Liste de locuteurs par échange atomique** — non fait, même raison.
4. ~~**Mixage vectorisable**~~ — **déclassé par la mesure.** 23,37 µs par
   trame à dix locuteurs, soit 0,12 % du budget : le vectoriser rendrait
   0,1 % de processeur. Le tampon circulaire à tranches contiguës reste
   souhaitable, mais comme **conséquence** du point 2 (sortir les verrous),
   pas comme objectif. À ne pas écrire pour lui-même.
5. ✅ **Canaux bornés, et `Bytes` transmis sans copie.** Le canal des
   datagrammes voix était **non borné** : un décodeur en retard faisait
   grandir la file sans limite. Il est plafonné à 128 trames, `try_send` ne
   bloque jamais la pompe de datagrammes, et une file pleine jette — une trame
   qui attendrait derrière deux secondes d'arriéré ne vaut plus rien. Les
   paquets voyagent désormais en `Bytes` du transport jusqu'au moteur, sans le
   `to_vec()` qui les recopiait un par un. Même traitement pour la file de
   capture (16 blocs).
6. ✅ **Priorité temps réel des fils audio.** `AvSetMmThreadCharacteristics`,
   classe **« Pro Audio »**, sur le fil de rendu comme sur celui de capture.
   C'est ce que fait tout moteur audio Windows sérieux et ce que nous ne
   faisions pas : sans cette inscription, nos fils sont ordonnancés comme
   n'importe quel fil de l'application, et un jeu qui sature les cœurs peut les
   laisser attendre — le craquement « quand je lance Valorant ». L'inscription
   est par fil et se défait au `Drop`, donc impossible à oublier sur un chemin
   d'erreur ; un refus de Windows n'est pas fatal et se dit une fois au journal.
7. ✅ **Chemin rapide au rapport 1,0** dans le rééchantillonneur. Il faisait
   l'interpolation cubique complète même sans rien à convertir — le cas le plus
   courant, puisque le moteur demande 48 kHz et que la plupart des cartes le
   donnent. La sortie est maintenant l'entrée **au bit près**, par recopie de
   tranches contiguës. Trois tests le couvrent, dont l'enroulement du tampon
   circulaire, seul cas où une recopie naïve se tromperait.

   **Mesuré : 5,29 µs → ~75 ns par trame, soit environ 70×.** C'est le seul
   chiffre spectaculaire de P4, et il porte sur le cas le plus fréquent.

   > **Ce que le banc dit d'autre, et qu'il faut lire avec prudence.** Le
   > mixage et les conversions réelles ressortent 5 à 9 % au-dessus de la
   > mesure de P1 — sur du code que ce lot n'a pas touché. Deux exécutions
   > consécutives concordent ensuite à ~1 % près, ce qui situe le bruit
   > intra-session bien en dessous de l'écart constaté : la différence tient
   > donc à l'état de la machine entre deux séances (fréquence, charge de
   > fond), pas au changement. Je ne l'ai pas poursuivie plus loin, et voici
   > pourquoi c'est défendable : le mixage pèse 0,12 % du budget audio, donc
   > 7 % de plus en pèsent 0,008 %. Reconstruire la référence pour trancher
   > coûterait quatre minutes de compilation pour un chiffre sans conséquence.

*Cible mesurable : zéro sous-alimentation (⚙ → Relevé de performance) pendant
une partie de Valorant, avec dix locuteurs simultanés et DeepFilterNet actif.*
**Cette cible n'est pas encore vérifiée** — elle demande une vraie machine, un
vrai jeu et de vraies oreilles. Le compteur est en place pour ça.

*Coût réel : une session.*

## P5 — Le serveur : payer une fois ce qu'on paie N fois ✅ (livré le 2026-08-27)

1. ✅ **Sérialiser une fois par message, pas par destinataire.** Le canal
   porte des `Arc<[u8]>` — du JSON, saut de ligne compris, prêt à écrire. Le
   garde-fou de longueur (`MAX_LINE`) vivait dans chaque tâche d'écriture ; il
   est désormais à l'endroit unique où la ligne naît.

   **Mesuré : 224,9 µs → 8,6 µs à 30 destinataires, soit 26×**, cohérent avec
   les 29,7× de P1 (les deux mesures encadrent le bruit de la machine).

2. ✅ **Historique indexé.** Un `(horodatage, position)` par message, construit
   par la lecture que le démarrage faisait déjà, complété par le fil
   d'écriture — seul à connaître la position d'une ligne qu'il vient d'écrire.
   `before()` fait une recherche binaire puis lit **les cinquante lignes de la
   page**, au lieu de relire les quinze mégaoctets du fichier, de désérialiser
   chaque ligne, de trier et de tout jeter sauf une page.

   Seize octets par message : cent mille messages tiennent dans 1,6 Mo.
   L'index est **trié**, et ce n'est pas cosmétique — l'horloge murale peut
   reculer (NTP, machine virtuelle qui se réveille), si bien que le fichier
   n'est pas nécessairement dans l'ordre du temps.

   > **Un défaut trouvé en écrivant le test, pas le code.** Le cache mémoire,
   > lui, restait dans l'ordre du *fichier* : une horloge qui recule mélangeait
   > la pagination des mille derniers messages. C'est un « mineur » de
   > `AUDIT.md` qui traînait ; `append` insère maintenant au bon endroit, et le
   > cache est trié au démarrage comme l'index.

   > **Une régression que j'ai introduite puis corrigée.** `read_line` échoue
   > sur de l'UTF-8 invalide **sans dire combien d'octets il a consommés** : ma
   > première version abandonnait donc tout le fichier après une ligne abîmée,
   > là où l'ancien code la sautait. Un test existant l'a attrapée. `read_until`
   > sur des octets bruts, puis `from_slice` qui valide l'UTF-8 lui-même, fait
   > les deux : on saute la ligne **et** on sait de combien avancer.

3. ✅ **Roster différentiel.** `MemberUpdate { member }` remplace la liste
   complète partout où une seule fiche change : connexion, déconnexion, entrée
   et sortie de vocal. La liste entière ne part plus qu'à la connexion de son
   destinataire, et aux remaniements qui touchent tout le monde.

   **Mesuré sur `ki-load`** — 20 clients, 14 auditeurs, 20 secondes, trois fois
   le même essai :

   | | Contrôle reçu | Rosters complets |
   |---|---|---|
   | Avant | 968 Kio | 504 |
   | Fiches sur le vocal seulement | 358 Kio | 117 |
   | Fiches sur la déconnexion aussi | **134 Kio** | **20** |

   Soit **7,2×**, et exactement un roster par client — celui de sa propre
   connexion. Le compteur de `ki-load` sert désormais de garde-fou : plus de
   rosters que de connectés, c'est qu'une diffusion complète subsiste quelque
   part.

   Un client antérieur ignore `MemberUpdate` — tous les `match` du protocole
   sont exhaustifs et tolèrent l'inconnu — et voit simplement la présence se
   rafraîchir moins souvent.

*Coût réel : une session.*

## P6 — Compilation et livraison ✅ (livré le 2026-08-27, deux points sur quatre)

1. ✅ **`target-cpu=x86-64-v2`.** Le binaire était compilé pour le x86-64
   **d'origine** — celui de 1999 : ni SSE4, ni POPCNT, ni rien de ce que les
   processeurs font depuis. Tout le calcul par échantillon s'exécutait en
   scalaire.

   **Mesuré**, machine au repos, contre les valeurs absolues de P4 :

   | | Avant | `x86-64-v2` | |
   |---|---|---|---|
   | Mixage, 1 locuteur | 2,45 µs | 2,17 µs | −11 % |
   | Mixage, 4 locuteurs | 9,89 µs | 8,34 µs | −16 % |
   | Mixage, 10 locuteurs | 24,86 µs | 20,05 µs | **−19 %** |
   | Rééchantillonnage 44,1 → 48 | 6,12 µs | 5,75 µs | −6 % |

   **Le gain croît avec le nombre de locuteurs**, ce qui est la signature
   d'une vectorisation : plus il y a d'échantillons à additionner, plus le
   travail par instruction compte. Pour une ligne de configuration.

   Le niveau **v3** (AVX2, Haswell 2013) donnerait davantage et se pose en
   changeant un seul mot. On ne le prend pas : une machine d'avant 2013
   refuserait alors de démarrer l'exécutable, avec un message de Windows que
   personne ne sait interpréter. Un gain mesurable ne vaut pas un utilisateur
   qui ne peut plus lancer l'application du tout.

   > **Une mesure jetée en chemin.** La première série annonçait le
   > rééchantillonnage **trois fois plus lent** — j'avais lancé une compilation
   > pendant le banc. C'est exactement la discipline que P1 devait installer,
   > et je l'ai enfreinte ; la mesure a été refaite à vide.

2. ✅ **Signature Ed25519 des releases**, plus le délai de téléchargement qui
   manquait.

   L'application **remplace son propre exécutable**, et la seule garantie
   d'intégrité était TLS jusqu'à GitHub : quiconque obtenait le droit de
   publier une release — compte compromis, jeton d'action fuité, actif
   remplacé après coup — exécutait du code arbitraire chez tout le monde. Le
   contrôle de taille qui existait n'attrape qu'un téléchargement tronqué.

   Le signeur est un **exemple du crate client** (`examples/signer.rs`), donc
   il partage exactement la même version d'`ed25519-dalek` que le code qui
   vérifie, dans le même `Cargo.lock` : une divergence entre signer et vérifier
   ne se verrait qu'en production, sur les machines des autres. Il est relu
   comme le reste et couvert par `clippy --all-targets`.

   Le téléchargement, lui, n'avait **aucun délai** : un serveur qui accepte la
   connexion puis cesse d'envoyer laissait le fil attendre pour toujours. Le
   nouveau délai borne le *silence*, pas la durée — une ligne lente aboutit,
   une ligne morte non.

   > **La vérification est écrite mais pas encore armée**, et c'est délibéré.
   > Tant que `RELEASE_PUBKEY_HEX` est vide, le client le dit dans ses traces
   > et poursuit — l'état d'avant, ni meilleur ni pire. L'armer avant que la
   > chaîne ne signe couperait toute mise à jour, y compris celle qui
   > apporterait le correctif. Trois gestes pour l'activer, décrits dans
   > `deploy/SIGNATURE.md` ; c'est **à toi** de les faire, la clé privée ne
   > devant passer par personne d'autre.

3. ❌ **PGO — écarté, et voici pourquoi.** L'optimisation guidée par le profil
   demande d'exécuter une charge représentative entre deux compilations. Or ce
   qu'il faudrait profiler, c'est un **client graphique** : l'arbre de widgets,
   la mise en page, le rendu. Le faire tourner sans écran sur un coureur
   d'intégration continue relève du bricolage, et un profil qui ne couvrirait
   que le DSP porterait sur 0,12 % du budget audio (mesuré en P1).

   S'ajoute un risque asymétrique : une chaîne de publication en deux passes
   qui casse, c'est plus de release du tout. Le gain espéré — 5 à 15 % sur du
   code dominé par des branches — ne vaut pas ça tant que rien ne montre que
   l'application est limitée par là. À reconsidérer si le relevé de
   performance (P1) désigne un jour un coupable précis.

4. ❌ **Mise à jour différentielle — non faite, et ce n'est pas un oubli.**
   Trente et un mégaoctets par mise à jour et par personne, c'est réel, mais ce
   n'est ni de la performance ni de la correction : c'est du confort de
   téléchargement, une fois par version. Le travail, lui, est sérieux —
   génération du correctif en intégration continue, application côté client,
   vérification du résultat, repli sur le téléchargement complet quand le
   correctif ne s'applique pas — et il **s'articule avec la signature** : c'est
   le binaire reconstruit qu'il faudrait vérifier, pas le correctif.

   L'ordre juste est donc : armer la signature (P6.2), la laisser tourner sur
   quelques versions, et seulement ensuite bâtir le différentiel par-dessus.

*Coût réel : une session pour les deux points livrés.*

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
| CPU client au repos, hors vocal | ~~20 images/s inconditionnelles~~ **5,39 %** d'un cœur | **1,72 %** — atteint (3,1×) |
| Temps de rendu du fil, 500 messages | proportionnel au nombre de messages | constant (virtualisé) |
| Allocations par image de rendu | plusieurs milliers (copies d'état) | ~ 0 en régime |
| Allocations par rappel audio | ~~1 (sortie), 2 (capture)~~ | **0** — atteint |
| Priorité des fils audio | ~~ordinaire~~ | **« Pro Audio » (MMCSS)** — atteint |
| Rééchantillonnage à l'identité 48 → 48 | ~~5,29 µs/trame~~ | **~75 ns** (≈70×) |
| File des datagrammes voix | ~~non bornée~~ | **128 trames**, jette au-delà |
| Verrous pris par le rappel de sortie | 4 à 5 par trame | 0 — *non fait, en attente de la mesure terrain* |
| Sous-alimentations pendant une partie | ~~non mesuré~~ **mesuré** (⚙ → Relevé) | 0 |
| Mixage, 10 locuteurs | 23,4 µs/trame — **0,12 % du budget** | *inchangé : rien à gagner* |
| Diffusion d'un `Members` (30 connectés) | ~~30 copies + 30 sérialisations~~ 224,9 µs | **8,6 µs** (26×) — atteint |
| Contrôle sur le fil, 20 clients qui se connectent | ~~968 Kio~~ | **134 Kio** (7,2×) — atteint |
| Page d'historique (salon de 100 k messages) | ~~relecture de ~15 Mo~~ | **recherche binaire + `seek`** — atteint |
| Tests exécutés avant publication | 0 | 132 |
| Mixage, 10 locuteurs (`x86-64-v2`) | ~~24,86 µs~~ | **20,05 µs** (−19 %) — atteint |
| Intégrité d'une mise à jour | ~~taille seule~~ | **signature Ed25519** — écrite, à armer |
| Poids d'une mise à jour | 31 Mo | quelques centaines de Ko — *non fait, voir P6.4* |

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
