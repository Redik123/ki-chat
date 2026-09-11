# Plan — le bot musique de ki-chat

Un membre de plus dans le salon vocal : « Musique ». Il joue ce que le
groupe lui demande, depuis YouTube et SoundCloud, et se pilote depuis une
bannière au-dessus du chat — pas de commandes texte, rien de Discord. Le
serveur fait tout ; le client n'a que l'interface.

Écrit le 2026-09-11, avant la première ligne de code, comme le plan
VALORANT. Ce qu'on a vérifié est marqué **vérifié** ; le reste est à
vérifier au jalon qui le concerne.

## Ce qu'on veut

- **Le serveur porte le bot.** Il tire le son, l'encode, l'émet dans le
  salon vocal comme n'importe quel membre, sur le circuit existant de la
  voix. Aucun client ne télécharge quoi que ce soit.
- **YouTube et SoundCloud** comme sources ; la recherche depuis ki-chat.
- **Une bannière** au-dessus de la zone de chat : repliée, une ligne —
  commandes de base et titre en cours ; déroulée, tout le détail — file
  d'attente, recherche, playlists, pochette.
- **Le contrôle aux modérateurs au minimum** : une permission
  « Contrôler la musique », donnée d'office au rôle Modérateur ; les autres
  voient tout et règlent leur propre volume.
- **Pas de pub** : le bot tire le flux audio brut, qui n'en contient pas.
  Un compte Google peut être nécessaire pour passer les contrôles
  anti-robot — un compte jetable, jamais le compte principal (voir Risques).

## Ce qu'on a trouvé

### L'extracteur : yt-dlp

L'outil de référence, maintenu, qui sait lire YouTube **et** SoundCloud
(**vérifié** dans sa documentation). Ce qui compte pour nous :

- **Un exécutable autonome** (`yt-dlp_linux`, x86_64, glibc 2.17+ ;
  `yt-dlp_linux_aarch64` pour l'ARM) — pas de Python à installer.
- **Un moteur JavaScript est requis pour YouTube** depuis 2025 : YouTube
  protège ses flux par des défis JavaScript que yt-dlp résout avec
  `yt-dlp-ejs`, embarqué dans l'exécutable autonome, mais qui a besoin d'un
  moteur — Deno 2.3+ (recommandé, ~100 Mo), Node 22+, ou QuickJS
  (2025-04-26+, ~2 Mo, plus lent). **Vérifié** (wiki EJS).
- **Sortie sur la sortie standard** : `-o -` ; **recherche** :
  `ytsearch10:<mots>` et `scsearch10:<mots>` avec `-j --flat-playlist`
  pour ne lire que les métadonnées (titre, durée, auteur, vignette) sans
  rien télécharger. **Vérifié** (README).
- **Cookies** : `--cookies fichier.txt` au format Netscape, exportés
  depuis un navigateur connecté. **Vérifié**.
- **Mise à jour intégrée** : `-U` remplace l'exécutable par la dernière
  version (canal `stable` mensuel, `nightly` quotidien recommandé). YouTube
  casse régulièrement les anciennes versions : le bot doit se mettre à jour
  seul. **Vérifié**.

### Les contrôles anti-robot de YouTube (PO tokens)

Depuis 2024, YouTube exige pour certains clients un « Proof of Origin
token » ; sans lui, un serveur en datacenter voit « Sign in to confirm
you're not a bot » ou des 403 sur le flux. **Vérifié** (guide PO Token du
wiki yt-dlp) : les clients `tv`, `web_embedded` et `android_vr` n'en
demandent pas ; `mweb` en demande un pour le flux ; les abonnés Premium en
sont dispensés. Il existe un fournisseur de tokens (`bgutil-ytdlp-pot-
provider`) qui tourne dans un conteneur à part.

Notre ordre de bataille : d'abord sans rien (clients `tv` /
`web_embedded`), puis avec les cookies d'un compte jetable, puis le
fournisseur de tokens si YouTube ferme encore. SoundCloud ne pose aucun de
ces problèmes et sert de source qui « marche toujours ».

### Le décodage : ffmpeg

yt-dlp donne un conteneur (WebM/Opus le plus souvent, M4A/AAC parfois).
ffmpeg le décode en PCM 48 kHz stéréo flottant, sur un tube :
`ffmpeg -i pipe:0 -vn -f f32le -ar 48000 -ac 2 pipe:1`. Un exécutable
statique (~80 Mo) suffit, pas de paquet Debian et ses 300 Mo de
dépendances.

### Ce que ki-chat a déjà (**vérifié** dans le code)

- **Le protocole voix** : un datagramme = en-tête `KV`, version, `user_id`
  de l'émetteur, compteur, puis la trame Opus chiffrée en XChaCha20-Poly1305
  avec la clé de session que le serveur remet à chacun dans `Welcome` ; le
  nonce vient de `(user_id, compteur)`. Le serveur relaie sans déchiffrer.
  **Il détient la clé** : il peut donc émettre lui-même, sous un
  identifiant réservé, des paquets que tous les clients déchiffrent comme
  ceux d'un membre.
- **Le relais** : `voice_routes` donne, par salon vocal, les connexions à
  qui envoyer ; le bot émet dans un salon en envoyant à ses pairs.
- **Le client** crée un récepteur par émetteur inconnu (jitter buffer,
  décodeur, volume par personne) : un émetteur de plus ne demande rien de
  neuf côté lecture. Le décodeur mono de la voix accepte une trame stéréo
  (libopus la mélange) — la stéréo vraie viendra avec celle du son du jeu.
- **ki-opus**, notre liaison libopus, compile en statique dans l'image
  Docker (le tarball est vérifié par empreinte) : le serveur peut encoder.
- **Le roster** : un membre virtuel « Musique » avec `voice = Some(salon)`
  apparaît dans le salon comme les autres, avec l'anneau « qui parle ».

## Lignes rouges

- **Jamais le compte Google principal** sur le serveur : un compte jetable,
  créé pour ça, dont le blocage ne coûte rien.
- **Les cookies ne quittent pas le serveur** : déposés dans le volume
  (`data/musique/cookies.txt`), jamais dans le dépôt, jamais dans un
  message, jamais dans les diagnostics.
- **Rien n'est stocké durablement** : le son passe en flux, aucun fichier
  audio n'est écrit sur disque (seules les vignettes sont mises en cache).
- **Le contrôle est une permission**, pas un rôle codé en dur : l'admin la
  donne à qui il veut, les modérateurs l'ont d'office.
- **Usage privé entre amis** : télécharger de YouTube reste contraire à ses
  conditions d'utilisation ; c'est la décision de l'admin du serveur, et le
  README le dit.

## Architecture

```
  bannière (client)  ──ClientMsg::Musique(commande)──▶  serveur
                     ◀──ServerMsg::MusiqueEtat/Resultats──   │
                                                             ▼
   yt-dlp -o - (url) ──▶ ffmpeg → PCM f32 48 kHz stéréo ──▶ cadenceur 20 ms
                                                             │
                                          ki-opus (stéréo, 96 kbit/s, 20 ms)
                                                             │
                              XChaCha20 (id « Musique », compteur) ──▶ pairs du salon
```

**Le lecteur** (serveur, `crates/server/src/musique.rs`) :

- un seul bot, dans un salon vocal à la fois (`salon: Option<ChannelId>`) ;
  il rejoint le salon du modérateur qui lance la lecture ;
- une file d'attente (`Vec<Piste>`), la piste en cours avec sa position,
  lecture/pause, un volume global (gain avant encodage) ;
- un fil par piste : `yt-dlp` puis `ffmpeg` en processus enfants, tubes
  reliés, PCM lu par blocs de 20 ms (960 échantillons × 2 canaux) dans un
  canal borné à une seconde — ffmpeg avance plus vite que le temps réel, le
  canal borné le retient ;
- un cadenceur `tokio::interval(20 ms)` qui prend un bloc, l'encode, le
  chiffre, l'envoie aux pairs du salon ; sans bloc (tube en retard), une
  trame de silence — jamais de trou ;
- fin de piste = fin du tube ; la suivante démarre dans la seconde ;
- **seul dans le salon plus de cinq minutes, le bot se met en pause** ; le
  salon vidé, il s'arrête.

**Les outils** (`data/musique/outils/`) : `yt-dlp` copié là au premier
démarrage depuis l'image (l'image est en lecture seule, le volume non), mis
à jour par `-U` au démarrage puis chaque nuit ; `ffmpeg` et `deno`
restent dans l'image. Chaque appel a un délai maximal et un plafond de
mémoire (`ulimit`), et un enfant qui traîne est tué — un extracteur cassé ne
doit jamais bloquer le serveur.

**La recherche** : `yt-dlp "ytsearch10:<mots>" -j --flat-playlist`
(`scsearch10:` pour SoundCloud), dix résultats, quelques secondes ; les
résultats sont mis en cache une heure par requête. Les **vignettes** sont
tirées par le serveur et servies par son HTTPS existant, en cache borné
(50 Mo) : les clients ne parlent qu'à ki-chat, jamais à YouTube.

**Le membre virtuel** : identifiant réservé `MUSIQUE_ID` (une constante du
protocole, hors de la plage des comptes), pseudo « Musique », présent dans
le roster avec `voice = Some(salon)` tant qu'il joue, `speaking = true`
pendant la lecture. Chacun le règle ou le coupe comme un membre.

## Interface : la bannière

Au-dessus de la zone de chat, visible quand le bot joue dans le salon
vocal où l'on est (et, en plus discret, quand il joue ailleurs : « Musique
joue dans #Général »).

**Repliée** — une ligne de 36 px :

    ▶ ⏸ ⏭   ━━━━━━●━━━━━   1:42 / 3:56   Daft Punk — Around the World   🔊 ▂▃▅   ˅

lecture ou pause, suivant, la progression avec les temps, le titre et
l'artiste (qui défilent s'ils dépassent), **ton** volume — réglable par
tout le monde puisqu'il ne concerne que tes oreilles — et le chevron.

**Déroulée** — un tiers de la fenêtre, le chat glisse dessous :

- à gauche la pochette en grand, le titre, l'artiste, la source, qui l'a
  ajoutée ;
- au centre la **file d'attente**, réordonnable à la souris (glisser),
  chaque ligne avec vignette, titre, durée, qui l'a ajoutée, une croix ;
- en haut un **champ de recherche** avec le choix YouTube / SoundCloud ;
  les résultats se listent dessous, un clic ajoute en fin de file, un
  autre bouton « jouer maintenant » ;
- à droite les **playlists du groupe** : enregistrer la file, charger,
  ajouter à la file ; et le **volume global** du bot, un curseur ;
- en bas : vider la file, faire venir le bot dans mon salon, l'arrêter.

Les boutons de contrôle sont **grisés sans la permission**, avec l'aide
« réservé aux modérateurs » ; la recherche et les playlists aussi. Le
volume perso et le chevron restent à tous.

## Droits

- Nouvelle permission `CONTROL_MUSIC` (bit suivant dans `perm`), dans
  `NOT_FOR_EVERYONE`, **donnée d'office au rôle Modérateur** à la
  migration, comme « Supprimer les messages ».
- Réglage serveur, dans le panneau admin : « les membres peuvent ajouter à
  la file » (oui/non, non par défaut). Quand oui, un membre ordinaire peut
  chercher et ajouter, pas piloter (lecture, pause, suivant, ordre, vider,
  volume global restent aux modérateurs).
- Tout ce qui pilote est **audité** (`musique.play`, `musique.skip`,
  `musique.clear`…), comme les actions d'administration.

## Stockage (`data/musique/`)

- `cookies.txt` : déposé par l'admin, jamais lu par un autre chemin que
  yt-dlp ;
- `playlists.json` : les playlists du groupe (nom, pistes : source, id,
  titre, artiste, durée, vignette) ;
- `file.json` : la file et la piste en cours, réécrites à chaque
  changement — un redémarrage reprend où il en était (en pause) ;
- `vignettes/` : cache borné ;
- `outils/yt-dlp` : l'exécutable auto-mis à jour.

## Protocole

```rust
ClientMsg::Musique(CommandeMusique)
enum CommandeMusique {
    Chercher { texte: String, source: Source },        // YouTube | SoundCloud
    Ajouter { piste: PisteRef, maintenant: bool },     // en fin de file, ou tout de suite
    Retirer { index: usize },
    Deplacer { de: usize, vers: usize },
    Lecture, Pause, Suivant,
    Vider,
    Volume { pour_cent: u8 },                           // global, modérateurs
    Rejoindre,                                          // le bot vient dans mon salon
    Arreter,
    PlaylistEnregistrer { nom: String },
    PlaylistCharger { nom: String, remplacer: bool },
    PlaylistSupprimer { nom: String },
}
ServerMsg::MusiqueEtat(EtatMusique)   // à chaque changement, et à la connexion
struct EtatMusique {
    salon: Option<ChannelId>,
    lecture: bool,
    en_cours: Option<Piste>, depuis_ms: u64, position_ms: u64,  // le client fait avancer la barre
    file: Vec<Piste>,
    volume: u8,
    playlists: Vec<String>,
    membres_ajoutent: bool,
}
ServerMsg::MusiqueResultats { pour: String, pistes: Vec<Piste> }
struct Piste { source, id, titre, artiste, duree_s, vignette: Option<String> /* URL ki-chat */, ajoute_par: Option<String> }
```

Tous les champs nouveaux sont optionnels pour les anciens clients ; un
client sans bannière voit simplement un membre « Musique » dans le salon
et l'entend.

## Budget et coûts

- **CPU serveur** : décodage ffmpeg d'un flux 128 kbit/s ≈ 2 % d'un cœur ;
  encodage Opus stéréo 48 kHz complexité 5 ≈ 3–5 % ; chiffrement
  négligeable. Une piste à la fois : ≈ 5–8 % d'un cœur. Une recherche =
  un yt-dlp de quelques secondes, en rafale limitée (une par seconde et par
  membre).
- **Réseau serveur** : 96 kbit/s par auditeur, comme une voix de plus.
- **Image Docker** : +30 Mo (yt-dlp) +80 Mo (ffmpeg statique) +100 Mo
  (deno) ≈ +210 Mo ; QuickJS à la place de deno ramène à +115 Mo mais
  résout les défis plus lentement — on commence avec deno, on mesure.
- **Mémoire** : un tube d'une seconde de PCM (384 Ko), les vignettes
  bornées, yt-dlp lui-même ≈ 100 Mo pendant qu'il tourne (`ulimit -v`).

## Jalons

### M1 — La chaîne — en test (2026-09-11)
yt-dlp, ffmpeg et deno dans l'image ; `musique.rs` lit une URL, décode,
encode, chiffre, émet dans le salon comme membre virtuel « Musique » ;
la permission `CONTROL_MUSIC` ; une commande minimale (Rejoindre + Ajouter
une URL + Arreter) depuis un champ provisoire dans le panneau admin.
**Validation** : une piste YouTube et une SoundCloud jouent dix minutes
dans le salon sans coupure, chacun règle son volume, le bot coupé chez
l'un ne l'est pas chez l'autre ; un yt-dlp tué en pleine piste ne fait pas
tomber le serveur.

### M2 — La file et la bannière
File d'attente, lecture/pause/suivant, recherche YouTube et SoundCloud,
vignettes par le serveur, la bannière repliée et déroulée, les boutons
grisés sans permission, l'audit. **Validation** : trente minutes de soirée
avec des ajouts à la volée par deux modérateurs, aucun blocage, la barre
de progression juste à la seconde, la bannière lisible en fenêtre étroite.

### M3 — Les playlists et le confort
Playlists du groupe, « les membres peuvent ajouter », glisser-déposer dans
la file, pause automatique quand le salon se vide, reprise après
redémarrage. **Validation** : une playlist de vingt pistes enregistrée,
rechargée après redémarrage du serveur, reprise à la bonne piste.

### M4 — La résilience
Auto-mise à jour de yt-dlp (démarrage + nuit), clients YouTube de repli,
cookies du compte jetable, fournisseur de PO tokens en conteneur si
YouTube ferme encore, compteurs dans `/diag-resume` (pistes jouées, échecs
par source, temps de démarrage d'une piste). **Validation** : une semaine
sans intervention ; une casse YouTube se répare seule à la mise à jour
suivante ou bascule sur les cookies.

### M5 — La voix
Piloter le bot à la voix, en pleine partie, sans lâcher la souris :
**commandes de base seulement** — suivante, pause ou coupe, reprends,
arrête, plus fort, moins fort. Pas de recherche de titre à la voix : le
vocabulaire ouvert demande de gros modèles et se trompe sur les titres,
le choix des morceaux reste à la bannière (décision du 2026-09-11).

Le plus léger possible, c'est la contrainte. Whisper n'est pas le bon
outil : il transcrit des phrases entières avec un gros modèle et coûte un
cœur par phrase. Le choix : **Vosk** (Kaldi) avec une **grammaire
fermée** — le mot d'appel suivi de chaque commande, plus un joker pour
tout le reste —, en flux continu, **ouvert seulement quand le VAD Silero
voit de la parole** : rien au silence, quelques pour cent d'un cœur en
parole, la commande reconnue pendant qu'on la dit. Tout tourne **chez
soi**, sur son propre micro ; le client envoie au serveur la même
commande que la bannière, les droits ne changent pas. Modèle
`vosk-model-small-fr` (~40 Mo) et bibliothèque (~10 Mo) téléchargés à
l'activation, option décochée par défaut, rien dans l'exécutable.

Deux modes, dans cet ordre : **une touche maintenue** (on tient la
touche, on parle, on lâche — aucune écoute de fond), puis **le mot
d'appel** pour qui a le CPU. Le mot d'appel se choisit tôt : deux
syllabes nettes, rares en conversation, pour ne pas se déclencher en
partie. Le bot confirme par un petit son, pas par une voix.
**Validation** : dix commandes d'affilée pendant une partie de VALORANT,
au moins neuf reconnues, aucun déclenchement sur la conversation
ordinaire d'une demi-heure, charge CPU mesurée avant et pendant.

En repli si Vosk coince à l'empaquetage : sherpa-onnx, qui a un détecteur
de mot d'appel dédié, plus lourd à embarquer.

## Risques et parades

- **YouTube bloque le serveur** (anti-robot, PO token) → clients sans
  token d'abord, cookies d'un compte jetable ensuite, fournisseur de tokens
  en dernier ; SoundCloud reste disponible ; le message d'erreur dans la
  bannière dit laquelle des trois marches manque.
- **yt-dlp casse** (YouTube change) → mise à jour automatique quotidienne
  sur le canal `nightly` ; le journal serveur dit la version.
- **Un enfant qui traîne** (yt-dlp ou ffmpeg bloqué) → délai maximal,
  `kill`, et la piste passe à la suivante avec une ligne dans la bannière.
- **CPU de l'hébergement** → une piste à la fois, complexité Opus
  réglable ; mesure au M1 sur le vrai serveur.
- **Compte Google bloqué** → il était jetable ; en refaire un.
- **Droits d'auteur** → usage privé, décision de l'admin, dit dans le
  README ; rien n'est stocké.

## Questions ouvertes

- Un seul bot pour tout le serveur, ou un par salon vocal ? **Un seul
  pour commencer**, nommé « Musique » sauf avis contraire ; la question
  revient si deux salons veulent de la musique en même temps.
- Le vote pour passer une piste (les non-modérateurs votent « suivant »,
  majorité des présents) — sympathique, à voir en M3.
- La stéréo chez l'auditeur : elle viendra avec celle du son du jeu (même
  chantier côté client) ; en attendant le bot est entendu en mono.
- Le nom et l'avatar du bot : « Musique » et une icône de note, ou un nom
  choisi par l'admin ?
