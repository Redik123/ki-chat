# ki-chat

Serveur de chat privé façon Discord, 100 % Rust, orienté gaming : chat texte
temps réel + vocal basse latence, pour ~30 personnes.

## Architecture

```
crates/
  protocol/     types partagés : messages de contrôle (JSON) + format paquets voix
  server/       ki-server : QUIC (contrôle + relais vocal SFU) + HTTP (fichiers)
  voice/        moteur audio client : cpal + Opus + jitter buffer (indépendant du transport)
  client-quic/  connexion QUIC cliente partagée (contrôle + datagrammes voix)
  client-cli/   client de test en ligne de commande (chat texte + vocal)
  client-gui/   ki-chat : l'application de bureau (egui) — chat, vocal, PTT global
                theme.rs (couleurs/typo), icons.rs (icônes dessinées),
                ui.rs (widgets), servers.rs (carnet de serveurs + sonde de ping)
```

**Plusieurs serveurs, un seul client.** L'écran d'accueil est un lanceur :
chaque serveur y est enregistré avec son adresse et les identifiants associés
(mot de passe mémorisé seulement si la case est cochée). Avant même de se
connecter, le client ouvre une poignée de main QUIC de test vers chaque
serveur enregistré et affiche son état et son ping — le serveur libère
aussitôt une connexion qui ne s'authentifie pas, rien n'est enregistré de son
côté.

**Photos de profil** : chacun règle la sienne et elle suit le compte, pas la
machine — on la retrouve depuis n'importe quel poste. Les vignettes ne
transitent pas dans la liste des membres : celle-ci ne porte qu'une empreinte
FNV-1a du contenu, et le client ne réclame (`RequestAvatars`) que ce qui
manque à son cache. Trente membres ne coûtent donc pas trente vignettes à
chaque changement de salon.

**L'identité d'un serveur appartient au serveur.** Son nom et son logo vivent
dans `data/server.json`, se règlent depuis le panneau d'administration
(`AdminSetServerInfo`, réservé aux admins) et sont distribués à tous les
membres — dans `Welcome` à la connexion, puis poussés par `ServerInfo` à
chaque changement. Un membre ordinaire ne peut pas les modifier : il ne
dispose que d'un **alias local**, un pense-bête qui ne change l'affichage que
sur sa machine. Le logo, lui, n'est modifiable nulle part côté client, ce qui
évite qu'un serveur puisse en imiter un autre sur le poste de quelqu'un.

**Transport : QUIC (TLS 1.3), une seule connexion par client.** Le contrôle
(auth, chat, présence, admin) passe sur un flux fiable, la voix en
datagrammes non fiables — sur le même port 9987/udp. Certificat auto-signé
généré au premier démarrage (persisté dans data/). Bénéfices : chiffrement
transport natif (plus besoin de reverse proxy TLS), reconnexion 0-RTT,
**migration de connexion** (changer de réseau sans être déconnecté), ping
RTT mesuré par le protocole. Le HTTP (port 8080) ne sert plus qu'au partage
de fichiers, pour que les liens restent ouvrables dans un navigateur.

**Salons textuels et salons vocaux, séparés.** Ouvrir un salon textuel ne
concerne que celui qui le lit : personne n'est prévenu, rien ne change pour
les autres. Le vocal, lui, se rejoint explicitement (`JoinVoice`) — se
connecter au serveur ne met plus personne dans un vocal d'office, et le micro
reste fermé tant qu'on n'y est pas entré. La liste de droite montre **tout le
monde sur le serveur**, avec le salon vocal occupé par chacun ; les occupants
d'un vocal apparaissent aussi sous son intitulé, à gauche.

**Chat texte** : JSON ligne à ligne sur le flux QUIC fiable. Historique
persisté en JSONL (un fichier
par salon textuel, les 1000 derniers messages en mémoire).

**Voix** : format de paquet maison (voir `protocol/src/lib.rs`), transporté
en datagrammes QUIC. Le client encode
en Opus et envoie des trames de 20 ms. Le serveur ne décode (ni ne déchiffre)
jamais : il authentifie le paquet par jeton, réécrit l'en-tête et relaie la
trame aux autres membres du salon (mode SFU). C'est l'approche
Mumble/TeamSpeak, pas la lourdeur WebRTC.

**Relais optimisé** : le relais tourne sur un **thread système dédié** (jamais
en concurrence avec le trafic de contrôle), route via une **table précalculée**
mise à jour aux join/leave (une seule lecture partagée par paquet), avec
buffers socket de 1 Mo et **marquage DSCP EF** (les routeurs DiffServ
priorisent la voix — appliqué aussi côté client). Le serveur mesure en plus
les **pertes montantes** de chaque émetteur (trous de compteurs) et les lui
signale toutes les 5 s — c'est ce qui alimente le **débit adaptatif** : en
mode « Auto » (défaut), le client baisse son débit Opus dès 5 % de pertes et
le remonte après 15 s propres.

**Moteur audio client** (`crates/voice`) : capture micro via cpal (WASAPI),
conversion mono + rééchantillonnage 48 kHz, encodage Opus 64 kbps (mode voip,
FEC intra-bande activée), jitter buffer de 40 ms par émetteur avec remise en
ordre et dissimulation de perte (PLC), mixage multi-émetteurs, keepalive UDP
pour la traversée NAT. Latence pipeline théorique : ~60-80 ms + réseau.
Flags de test sans matériel audio : `--tone` (émet une sinusoïde 440 Hz) et
`--deaf` (ne lit pas l'audio reçu).

## Sécurité

**Comptes** : pseudo + mot de passe, hachés en Argon2id (`data/users.json`).
La création de compte exige le code d'invitation du serveur (`KI_TOKEN`) :
au premier login d'un pseudo inconnu avec le bon code, le compte est créé.
Ensuite le code n'est plus nécessaire. Un compte ne peut être connecté
qu'une fois à la fois.

**Anti-force brute** : les tentatives d'authentification sont limitées par
adresse IP **et** par compte, avec un délai qui double à chaque échec au-delà
de cinq (2 s, 4 s, 8 s… plafonné à une minute, oublié après quinze minutes
sans échec). Un délai croissant plutôt qu'un verrouillage : sinon n'importe
qui bloquerait le compte d'un autre en échouant exprès. Le refus tombe
**avant** le hachage — c'est ce qui compte, car chaque essai déclencherait
sinon un Argon2id, volontairement coûteux : quelques centaines d'essais
simultanés satureraient la machine sans la moindre chance de trouver le mot
de passe. Les hachages tournent en outre sur le pool bloquant, pour ne pas
figer la boucle réseau.

**Bornes sur les entrées** : tout ce qui vient du réseau est borné, à
commencer par la ligne du flux de contrôle elle-même (160 Kio) — un lecteur
de lignes ordinaire fait grandir son tampon sans limite, ce qui épuiserait la
mémoire d'en face avant même l'authentification. Ensuite : messages
(4000 caractères), pseudos, mots de passe, codes d'invitation. Les textes
reçus perdent leurs caractères de contrôle et les **commandes
bidirectionnelles** Unicode, qui inversent le sens d'affichage et permettent
de faire lire à l'écran autre chose que ce qui est écrit. Les vignettes sont
validées structurellement (voir plus bas). Client et serveur appliquent les
mêmes règles, définies une seule fois dans `ki-protocol`.

**Vignettes (logos, photos)** : leur charpente PNG est vérifiée sans être
décodée — signature, enchaînement des blocs, dimensions bornées à 256 px, et
fin de fichier exacte. Seuls les blocs porteurs de pixels sont admis : ni
métadonnées, ni données collées après la fin. Sans quoi une vignette
parfaitement valide pourrait convoyer des octets arbitraires que le serveur
redistribuerait à tout le salon. Le décodeur est de surcroît borné en
dimensions et en allocation, ce qui neutralise les bombes de décompression.

**Mots de passe mémorisés côté client** : jamais en clair sur le disque. Le
client les confie au coffre natif du système — DPAPI sur Windows, qui dérive
la clé de la session de l'utilisateur sans jamais l'exposer au processus. Le
blob stocké est donc illisible sur une autre machine **et** sous un autre
compte du même poste, même si le fichier de configuration est copié.
`crates/client-gui/src/secret.rs` isole ce mécanisme derrière une porte
unique (`protect` / `reveal` / `available`) : brancher le Trousseau macOS/iOS
ou le Keystore Android sera un `#[cfg]` de plus, pas une refonte. Là où aucun
coffre n'existe, on refuse simplement de mémoriser — pas de repli en clair.

> Dériver la clé d'un identifiant matériel (adresse MAC, numéro de série) a
> été écarté : ces valeurs ne sont pas secrètes — la MAC est diffusée dans
> chaque trame réseau — donc la clé voyagerait avec le coffre ; elles changent
> (Windows randomise la MAC Wi-Fi par réseau, un dock ou un VPN en ajoute) ;
> et Android comme iOS renvoient une MAC bidon à toutes les applications.

**Voix chiffrée** : chaque trame Opus est scellée en XChaCha20-Poly1305 avec
une clé de session distribuée aux clients authentifiés (régénérée à chaque
démarrage du serveur). Le nonce dérive de (user_id, compteur de paquet). Le
serveur relaie sans jamais déchiffrer ; un paquet altéré ou forgé est rejeté
par les destinataires (compteur `rejetés` dans /stats).

**Transport chiffré natif** : toute la connexion (contrôle ET voix) passe
dans le tunnel TLS 1.3 de QUIC — plus besoin de reverse proxy. Le certificat
du serveur est auto-signé (les clients d'un serveur privé ne vérifient pas
la chaîne : protection totale contre l'écoute passive ; la voix reste en
plus chiffrée de bout en bout, donc même le serveur ne peut pas l'écouter).
L'ancienne limite « jeton voix en clair dans l'en-tête UDP » a disparu avec
le transport : un datagramme n'est accepté que sur la connexion authentifiée
de son émetteur.

Le seul morceau resté en clair est le **port HTTP 8080**, qui ne sert qu'au
partage de fichiers (pour que les liens s'ouvrent dans un navigateur). Un
reverse proxy TLS devant ce port reste donc possible si tu tiens à des liens
en `https://` avec un vrai domaine — mais c'est facultatif et sans effet sur
le chat ni sur la voix, qui ne passent plus par HTTP du tout.

## Lancer

```bash
# serveur (dev)
cargo run -p ki-server

# l'application de bureau
cargo run -p ki-client-gui

# ou le client de test en CLI (chat + vocal)
cargo run -p ki-client-cli -- 127.0.0.1 mon_pseudo mon_pass --invite changeme
```

Adresse serveur côté clients : `hôte` ou `hôte:port` (port QUIC, 9987 par
défaut) — plus d'URL `ws://`.

L'application retient serveur/pseudo et les réglages audio entre les
sessions. Le push-to-talk est **global** : la touche fonctionne même quand la
fenêtre n'a pas le focus (poll clavier système via GetAsyncKeyState), donc en
plein jeu. Modes : micro ouvert ou push-to-talk, touche configurable.

**Réglages audio** (bouton ⚙ Audio) — tout est appliqué à chaud et mémorisé :
- périphériques d'entrée/sortie (avec actualisation de la liste) ;
- vumètre micro en direct, gain d'entrée 0–200 % ;
- trois modes : micro ouvert, push-to-talk (touche globale), **activation
  vocale** avec seuil réglable (jauge verte quand la voix déclenche,
  maintien de 400 ms pour ne pas couper les fins de mots) ;
- **suppression de bruit à 3 niveaux** : désactivée / RNNoise (léger) /
  **DeepFilterNet3** — réseau de neurones qualité studio (inférence tract
  100 % Rust, modèle embarqué ~2 Mo, ~1 ms de CPU par trame, +30 ms de
  lookahead), le niveau « Krisp » en local ;
- **porte de bruit** réglable (0–10 %) : coupe tout résidu sous le seuil,
  ouverture rapide / fermeture douce ;
- **calibration automatique** (🎯, 5 s) : mesure le niveau ambiant — bruit de
  fond, autre personne dans la pièce — et règle la porte et le seuil
  d'activation juste au-dessus (la porte tournant avant l'AGC, une voix
  tierce sous le seuil est coupée au lieu d'être amplifiée) ;
- **gain automatique (AGC)** avec niveau cible réglable : attaque rapide
  anti-saturation, détente lente anti-pompage, gel sur le silence ;
- activation vocale : seuil ET durée de maintien réglables (100–1000 ms) ;
- push-to-talk : délai de relâchement réglable (0–500 ms) ;
- test micro « s'écouter » : **aller-retour complet par le codec Opus** au
  débit courant — on entend exactement ce que les autres entendent,
  artéfacts de compression compris (borné à 250 ms) ;
- volume de sortie global 0–200 %, avec **limiteur doux** (écrêtage progressif
  au-dessus de 85 % : plusieurs voix fortes saturent en douceur) ;
- débit Opus réglable 24–128 kbps, à chaud ;
- **tampon de gigue** : auto (adaptatif) ou fixé de 40 à 160 ms ;
- stats réseau en direct : ping, gigue max, perdus/récupérés FEC/rejetés.

**Résilience réseau** :
- **jitter buffer adaptatif** : la gigue d'arrivée est mesurée en continu
  (EWMA façon RFC 3550) et la latence de tampon s'ajuste seule, de 40 ms sur
  un réseau propre à 160 ms en Wi-Fi chaotique, avec rattrapage anti-dérive ;
- **récupération FEC réelle** : une trame perdue est reconstruite à partir des
  données de redondance du paquet suivant (et non plus seulement masquée) ;
- **ping vocal en direct** dans la barre du bas, mesuré sur le vrai chemin UDP
  (keepalive horodaté, écho serveur) — vert < 30 ms, orange < 80, rouge au-delà ;
- **vumètres par locuteur** dans la liste des membres pendant qu'ils parlent.
**Rôles** : le premier compte créé sur le serveur est admin — clic droit sur
un membre pour l'expulser (badge ♛ dans la liste). **Fichiers** : bouton 📎
dans le chat (25 Mo max), upload authentifié par le jeton de session, les
liens sont cliquables dans le chat.

**Panneau admin** (bouton ♛ Admin, visible pour les admins) :
- **Invitations** : génération de codes à usage unique (`ki-xxxxxxxxxx`) à
  distribuer aux nouveaux — le code maître `KI_TOKEN` reste valable mais n'a
  plus besoin de circuler. Boutons de copie (code seul ou avec l'adresse du
  serveur).
- **Comptes** : liste complète avec statut en ligne/bloqué, réinitialisation
  de mot de passe (un admin ne peut pas modifier un autre admin, mais peut se
  modifier lui-même), blocage/déblocage (un compte bloqué est expulsé sur le
  champ et ne peut plus se connecter ; les admins ne sont pas blocables).

Commandes CLI équivalentes : `/admin`, `/invite`, `/resetpw <pseudo> <mdp>`,
`/ban <pseudo>`, `/unban <pseudo>`, `/kick <id>`, `/passwd <ancien> <nouveau>`.

**Mon compte** : clic sur son propre pseudo (liste des membres ou coin bas
gauche) → changement de son mot de passe (l'ancien est vérifié).

**Volume par utilisateur** : clic droit sur un membre → slider 0–200 %.
Le réglage est appliqué au mixage en direct, et mémorisé côté client par
compte : tes réglages n'appartiennent qu'à toi et reviennent à chaque session
(0 % = couper quelqu'un ; les admins gardent leur volume indépendamment).

Une fois connecté : `/join 1` pour rejoindre le salon général, `/mic on` pour
parler, `/stats` pour les statistiques voix, `/quit` pour sortir.

> Build : libopus est compilé depuis les sources via cmake. Sur cette machine,
> `.cargo/config.toml` pointe vers le cmake embarqué dans Visual Studio 2026
> (le générateur suit le compilateur le plus récent installé) et fixe
> `CMAKE_POLICY_VERSION_MINIMUM=3.5` pour la compatibilité cmake 4.x.
> Rien à configurer : `cargo build` suffit, dans n'importe quel terminal.

Variables d'environnement du serveur :

| Variable       | Défaut     | Rôle                          |
|----------------|------------|-------------------------------|
| `KI_TOKEN`     | `changeme` | code d'invitation (création de comptes) |
| `KI_HTTP_PORT` | `8080`     | port HTTP (partage de fichiers) |
| `KI_UDP_PORT`  | `9987`     | port QUIC (contrôle + voix)   |
| `KI_DATA_DIR`  | `./data`   | persistance (comptes, historique, certificat TLS, identité du serveur) |

Build de prod : `cargo build --release` (LTO fat, binaire strippé).

## Coûts

**Licences : 0 €.** Opus est libre de redevances (BSD), tout l'écosystème Rust
utilisé est MIT/Apache-2.0. Il n'y a rien à payer côté logiciel.

Coûts réels pour ~30 personnes :

- VPS 2 vCPU / 4 Go (Hetzner CX22, OVH, Contabo…) : **~5–8 €/mois** — très
  largement suffisant, le serveur ne décode pas l'audio.
- Nom de domaine : ~10 €/an (optionnel, une IP suffit pour un serveur privé).
- TLS : 0 € — QUIC apporte son propre TLS 1.3 avec un certificat auto-signé
  généré au premier démarrage. Ni autorité de certification, ni reverse proxy.
- Auto-hébergement à la maison : 0 €/mois (prévoir ouverture de ports).

## Feuille de route

- [x] **M0** — fondations : protocole, serveur chat WS, relais vocal UDP, client CLI
- [x] **M1** — client audio : capture/lecture (cpal), encodage Opus, jitter buffer, toggle micro
- [x] **M2** — interface graphique egui : connexion mémorisée, salons, chat, présence, indicateurs "qui parle", push-to-talk global à touche configurable, niveau micro
- [x] **M3** — sécurité : comptes Argon2id sur invitation, voix chiffrée XChaCha20-Poly1305, WebSocket sécurisé derrière un reverse proxy *(rendu caduc par M5 : QUIC apporte son propre TLS)*
- [x] **M4** — confort : suppression de bruit RNNoise (activable à chaud), choix des périphériques audio dans l'appli, rôles (1er compte = admin, expulsion au clic droit), partage de fichiers (25 Mo max, liens cliquables)
- [x] **M4.5** — panneau admin : codes d'invitation à usage unique, réinitialisation de mot de passe, blocage/déblocage de comptes
- [x] **M4.6** — mon compte (changement de son propre mdp) + volume par utilisateur (clic droit, mémorisé par compte côté client)
- [x] **M4.7** — panneau audio complet : vumètre, gains entrée/sortie, activation vocale à seuil, test micro/sortie, débit Opus réglable
- [x] **M4.8** — voix niveau pro : AGC, jitter buffer adaptatif, récupération FEC, limiteur doux, ping vocal en direct, vumètres par locuteur
- [x] **M4.9** — DeepFilterNet3 (débruitage neuronal studio), porte de bruit, moniteur codec, cible AGC / maintien VAD / relâchement PTT / jitter réglables, stats réseau détaillées
- [x] **M4.10** — serveur optimisé : table de routage précalculée, mesure des pertes montantes par émetteur, débit Opus adaptatif (mode Auto)
- [x] **M5** — migration QUIC : une connexion unique TLS 1.3 (flux contrôle + datagrammes voix), certificat auto-signé persistant, RTT natif, reconnexion 0-RTT, migration réseau sans coupure, moteur audio découplé du transport
- [x] **M6** — refonte de l'interface : thème maison (`theme.rs`), jeu d'icônes vectorielles dessinées (`icons.rs` — les polices d'egui ne couvrent pas `●`, `↑`, `✕` : ils sortaient en carrés vides), avatars par pseudo, messages groupés avec séparateurs de journée, barre vocale et panneaux redessinés, icône d'application générée
- [x] **M6.1** — carnet de serveurs (`servers.rs`) : plusieurs serveurs nommés dans un seul client, identifiants (et mot de passe, si demandé) mémorisés par serveur, état et ping mesurés **avant** de se connecter par une poignée de main QUIC de test
- [x] **M6.5** — salons textuels et vocaux distincts : le vocal se rejoint à la demande, plus à la connexion ; liste des connectés au serveur en colonne de droite, occupants affichés sous chaque salon vocal
- [x] **M6.4** — photos de profil : chacun choisit la sienne depuis « Mon compte », le serveur la range dans le compte (`data/users.json`) et la diffuse ; la liste des membres ne porte qu'une empreinte, le client ne réclame que les vignettes qui lui manquent
- [x] **M6.3** — mots de passe mémorisés scellés par le coffre natif (DPAPI sur Windows), derrière une abstraction prête pour le Trousseau macOS/iOS et le Keystore Android ; les anciens mots de passe en clair sont chiffrés au chargement et effacés du fichier
- [x] **M6.2** — identité de serveur : nom + logo persistés côté serveur (`data/server.json`), réglés par les admins et poussés à tous les membres ; vignette PNG 64×64 aux coins arrondis, monogramme coloré à défaut. Côté client, seul un **alias local** est modifiable — le logo ne l'est pas, pour qu'un serveur ne puisse pas en imiter un autre
- [ ] **M5** — idées : overlay en jeu, chiffrement de l'en-tête voix, aperçu d'images dans le chat, plusieurs serveurs dans l'appli
