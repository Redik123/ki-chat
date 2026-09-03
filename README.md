# ki-chat

Serveur de chat privé façon Discord, 100 % Rust, orienté gaming : chat texte
temps réel, vocal basse latence, partage d'écran avec le son du jeu, overlay
« qui parle » — pour ~30 personnes.

## Architecture

```
crates/
  protocol/     types partagés : messages de contrôle (JSON), paquets voix,
                en-têtes des trames vidéo (KF) et du son du jeu (KA)
  server/       ki-server : QUIC (contrôle + relais vocal SFU + relais vidéo SFU)
                + HTTPS (fichiers, diagnostics)
  voice/        moteur audio client : WASAPI natif (cpal en secours), Opus, jitter
                buffer, annulation d'écho, docteur audio, son du jeu (indépendant
                du transport)
  video/        capture d'écran (Windows Graphics Capture), réduction d'image,
                encodage H.264 — NVENC sans SDK, openh264 en secours
  ki-opus/      libopus 1.6.1 compilé depuis les sources (DRED, Deep PLC, OSCE)
  ki-aec/       annulation d'écho acoustique (annulateur MDF de SpeexDSP)
  client-quic/  connexion QUIC cliente partagée (contrôle + datagrammes)
  client-cli/   client de test en ligne de commande (chat texte + vocal)
  load/         ki-load : N clients virtuels pour charger un serveur
  client-gui/   ki-chat : l'application de bureau (egui) — chat, vocal, diffusion
                theme.rs (couleurs/typo), icons.rs (icônes dessinées), ui.rs
                (widgets), servers.rs (carnet de serveurs + sonde de ping),
                net.rs (la connexion et son aiguillage), partage.rs (diffusion
                et visionnage), overlay.rs (« qui parle » par-dessus le jeu),
                ptt.rs (touche globale), secours.rs (journal, panic, relance),
                update.rs (mise à jour signée), perf.rs (relevé de performance)
```

Le chantier du partage d'écran a son propre document, [`PLAN-STREAM.md`](PLAN-STREAM.md) :
décisions, jalons, mesures et écarts assumés.

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
RTT mesuré par le protocole. Le port 8080 (HTTPS) ne sert plus qu'au partage
de fichiers, pour que les liens restent ouvrables dans un navigateur.

**Salons textuels et salons vocaux, séparés.** Ouvrir un salon textuel ne
concerne que celui qui le lit : personne n'est prévenu, rien ne change pour
les autres. Le vocal, lui, se rejoint explicitement (`JoinVoice`) — se
connecter au serveur ne met plus personne dans un vocal d'office, et le micro
reste fermé tant qu'on n'y est pas entré. La liste de droite montre **tout le
monde sur le serveur**, avec le salon vocal occupé par chacun ; les occupants
d'un vocal apparaissent aussi sous son intitulé, à gauche.

**Chat texte** : JSON ligne à ligne sur le flux QUIC fiable. Historique
persisté en JSONL (un fichier par salon textuel, les 1000 derniers messages
en mémoire). Le fil **se remonte** : arrivé en haut, le client réclame la
page précédente (`HistoryBefore`), qui vient s'ajouter au-dessus sans
écraser ce qui est affiché — les messages antérieurs restaient jusqu'ici
conservés sur disque mais inatteignables. Le serveur relit le fichier
au-delà de ce qu'il garde en mémoire, et annonce quand le début du salon
est atteint.

**Clic droit sur un message** — comme sur Discord : **répondre** (l'original
est rappelé au-dessus de la zone de saisie, puis sous la réponse, et un clic
y ramène), **réagir** d'un emoji (pastilles sous le message avec le compte,
la sienne se distingue et se retire d'un clic), **supprimer** — les siens,
chacun peut ; ceux des autres demandent la permission « Supprimer les
messages », que les modérateurs ont d'office, et le journal d'audit dit qui
a retiré quoi. Le fichier d'historique reste append-only : réactions et
suppressions sont des lignes d'événements, rejouées au démarrage, et un
message supprimé ne se rend plus à personne — pas même à la recherche.

**Voix** : format de paquet maison (voir `protocol/src/lib.rs`), transporté
en datagrammes QUIC. Le client encode
en Opus et envoie des trames de 20 ms. Les conversions de fréquence
(44,1 ⇄ 48 kHz) passent par une interpolation **cubique de Hermite** et non
linéaire : mesuré sur une sinusoïde à 1 kHz, l'erreur tombe d'un facteur ~53,
et l'échantillon de passé conservé d'un bloc à l'autre supprime le
craquement périodique que produisait le premier ordre. Le serveur ne décode
(ni ne déchiffre)
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

**Moteur audio client** (`crates/voice`) : capture et lecture par un **moteur
WASAPI natif** écrit pour l'occasion — le même chemin que Discord, cpal ne
servant plus que de secours —, conversion mono + rééchantillonnage 48 kHz,
encodage Opus 64 kbps (mode voip, FEC intra-bande activée), jitter buffer
adaptatif par émetteur avec remise en ordre et dissimulation de perte (PLC),
mixage multi-émetteurs, keepalive UDP pour la traversée NAT. Latence pipeline
théorique : ~60-80 ms + réseau. Flags de test sans matériel audio : `--tone`
(émet une sinusoïde 440 Hz) et `--deaf` (ne lit pas l'audio reçu).

Le moteur natif est né des jeux qui rebrassent l'audio (voir le docteur
audio, plus bas) : noms de périphériques tolérants à la ré-énumération USB,
réouverture sur un flux devenu muet, empreinte du périphérique relevée toutes
les 3 s (un casque « rebranché par logiciel » quand Valorant ou Synapse
l'ont rebrassé), redémarrages rangés en file, et **survie à la coupure
réseau** : le moteur voix vit hors de la connexion, une reconnexion ne
referme pas le micro. Le périphérique par défaut est demandé à Windows par
le rôle des **jeux** (`eConsole`), pas celui des appels : la catégorie
« communications » faisait baisser le son du jeu de 80 % chez certains, par
à-coups, au rythme des silences de la sortie.

**Annulation d'écho** (`crates/ki-aec`) : quelqu'un écoute sur haut-parleurs,
son micro capte les autres, chacun s'entend revenir. L'annulateur (MDF de
SpeexDSP) connaît ce qui vient d'être joué et soustrait son trajet acoustique
de la capture — le son du jeu d'un stream, mixé dans la même sortie, y passe
aussi.

## Partage d'écran

**Diffuser son écran** — bouton dans la barre vocale ou ⚙ → Diffusion
d'écran — et **regarder celui des autres** — clic droit sur un membre, ou
sur le petit écran affiché à côté de son pseudo pendant qu'il diffuse. La
diffusion est réservée au salon vocal du streamer, deux par salon au plus.

**Capture** : Windows Graphics Capture (celle de la barre de jeu Xbox et
d'OBS), écran entier ou fenêtre, curseur au choix. Chaque option n'est
demandée à Windows que s'il sait la faire — bordure jaune, cadence minimale
(Windows 11 24H2 seulement) : les mêmes réglages faisaient échouer la
capture sur un Windows 10, « la diffusion ne marche pas pour tout le
monde ». La cadence est tenue par nous, avant toute copie.

**Encodage H.264** : **NVENC** sur les cartes NVIDIA — sans SDK ni CUDA à
installer, l'API vidéo du pilote est chargée à la main (`nvEncodeAPI64.dll`),
3,9 ms par image en 1080p sur une RTX 3080 — et openh264 (profil contenu
d'écran temps réel) partout ailleurs, ou au choix dans les réglages. Avant
l'encodeur, l'image peut être réduite (plafond de hauteur) par moitiés puis
bilinéaire séparable. Réglages : source, résolution plafond, 15/30/60 i/s,
débit 1–20 Mbit/s (6 par défaut), curseur, aperçu local, encodeur. Tout
change à chaud, le stream continue, séquence comprise.

**Transport** : une trame = **un flux QUIC unidirectionnel**, sur la même
connexion (rien de plus à ouvrir que 9987/udp), priorité à la plus ancienne.
Fiable par trame, sans blocage de tête de ligne entre trames ; la
fragmentation en datagrammes a été disqualifiée à l'étude (19 % d'échec des
trames clés à 1 % de perte, et l'éviction de la voix). Chaque trame est
scellée en XChaCha20-Poly1305 sous une **clé générée par le streamer** et
remise aux seuls spectateurs acceptés ; l'en-tête clair (32 octets) est
l'AAD. Le serveur relaie sans déchiffrer : une tâche par spectateur nourrie
par une file de deux trames — un spectateur lent voit SES trames jetées et
repart d'une trame clé (demandée au streamer, une fois par demi-seconde au
plus), les autres ne s'en aperçoivent pas. Une trame clé annule celles
d'avant encore en vol (`RESET_STREAM`), une écriture qui bloque plus de
400 ms est abandonnée, la fenêtre d'envoi est bornée à 1 Mio et le contrôle
de congestion est BBR : c'est ce qui manquait quand la vidéo en retard
faisait faire la queue à la voix de tout le salon.

**Le son du jeu** voyage avec l'image : la **boucle par processus** de
Windows (10 2004 et plus) capture tout ce que joue le PC **sauf ki-chat** —
les spectateurs n'entendent pas leurs propres voix en retour —, Opus stéréo
96 kbit/s en trames de 20 ms, datagrammes « KA » chiffrés sous la même clé,
relayés tels quels. Chez le spectateur il est mixé dans la sortie du moteur
vocal (même volume, même annulateur d'écho), avec un curseur « son du jeu »
dans la fenêtre de visionnage. Case « Son du jeu dans le stream » chez le
streamer, cochée par défaut.

**Visionnage** : une fenêtre flottante par stream, croix pour partir,
décodage sur un fil dédié (départ sur trame clé, remise en ordre, saut
immédiat à une trame clé quand un trou se creuse), repeint au rythme du
stream. Pendant une diffusion, une ligne de statistiques part au journal
toutes les 10 s — images par seconde, kbit/s, images sautées à l'encodage,
jetées au réseau — c'est ce que les diagnostics remontent quand « ça lag ».

Reste à faire, dans cet ordre : le débit qui s'adapte de lui-même à la
connexion la plus courte du salon, la synchronisation image/son par
horodatage (transmis, pas encore exploité), la stéréo chez le spectateur.

## Overlay « qui parle »

Dans le jeu, savoir qui parle sans quitter la partie : une petite fenêtre à
part, toujours au-dessus, sans bordure, **transparente aux clics et jamais
active** — le jeu ne sait pas qu'elle existe, l'anti-triche non plus, rien
n'est injecté nulle part. Un rond par voix (l'avatar, le pseudo si on le
demande), rien au silence, dans le coin de son choix. Elle ne se montre que
lorsque ki-chat n'a pas le focus et qu'on est en salon vocal, et se repeint
dix fois par seconde quand quelqu'un parle, jamais plus.

**Désactivé de base** : ⚙ → Overlay en jeu, « Afficher qui parle par-dessus
le jeu » pour l'allumer. Il se voit partout où le jeu passe par le compositeur de Windows
— fenêtré, fenêtré sans bordure, et « plein écran » avec les optimisations
plein écran, le défaut. En **plein écran exclusif** (Valorant réglé sur
« Plein écran » plutôt que « Fenêtré plein écran »), rien ne peut passer
au-dessus : l'overlay le détecte et le dit dans ses réglages.

La transparence n'est pas celle qu'on croit : une fenêtre transparente aux
clics est « en couches » pour Windows, et cette couche ignore la
transparence du rendu — d'où un carré noir au premier essai. La fenêtre est
donc **découpée à la forme de ses ronds** (`SetWindowRgn`) : entre eux, il
n'y a pas de fenêtre du tout. Et certains jeux se remettent au-dessus sans
arrêt : l'overlay fait pareil, sans jamais prendre le focus.

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

**Anti-spam du chat** : un seau à jetons par connexion — 5 messages par
seconde en régime établi, avec une réserve de 10 pour les rafales normales
(coller cinq lignes d'affilée est un usage courant, en écrire cinquante ne
l'est pas). Au-delà, le message est refusé sans être diffusé. Sans cette
borne, un client modifié saturait d'un coup la mémoire glissante, le fichier
d'historique et la bande passante de tout le monde.

**Écritures atomiques** : `users.json`, `server.json` et les autres fichiers
d'état sont écrits à côté puis renommés. `rename` étant atomique sur NTFS
comme sur ext4, le chemin final désigne à tout instant soit l'ancien contenu
complet, soit le nouveau. Une coupure de courant pendant une sauvegarde
laissait auparavant un fichier à zéro octet — et le serveur refusait de
redémarrer, tous les comptes avec.

**Bornes sur les entrées** : tout ce qui vient du réseau est borné, à
commencer par la ligne du flux de contrôle elle-même (160 Kio) — un lecteur
de lignes ordinaire fait grandir son tampon sans limite, ce qui épuiserait la
mémoire d'en face avant même l'authentification. Ensuite : messages
(4000 caractères), pseudos, mots de passe, codes d'invitation, motifs de
modération (200 caractères). Les textes
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

Le partage de fichiers (**port 8080**) est lui aussi en TLS, avec le même
certificat que le QUIC : le contenu des fichiers et le jeton de session ne
voyagent plus en clair. Le client vérifie la même empreinte des deux côtés.

Cette empreinte est justement ce qui authentifie le serveur : un certificat
auto-signé n'est contresigné par personne, alors le client la **retient à la
première connexion** et refuse ensuite toute identité différente — comme SSH.
Sans cela, quiconque sur le trajet pouvait se faire passer pour le serveur et
lire le premier message, celui qui porte le mot de passe.

Deux conséquences pratiques. Un navigateur avertit une fois que le certificat
est auto-signé, quand on ouvre un lien de fichier à la main : c'est attendu,
et un reverse proxy TLS avec un vrai domaine l'évite si tu y tiens. Et si tu
réinstalles le serveur en perdant `data/quic-cert.der`, son empreinte change :
les clients refuseront de se connecter tant que le serveur n'aura pas été
retiré puis rajouté à leur carnet. Sauvegarde ce fichier avec le reste.

## Diagnostics partagés

Chaque joueur peut cocher « Partager mes diagnostics avec l'admin du
serveur » (⚙ → Aide & diagnostics). Son client envoie alors toutes les
10 minutes son
**journal technique** — périphériques ouverts/perdus, réouvertures, version,
rapport du docteur audio, ce que la diffusion d'écran a à dire (encodeur
retenu, capture bridée, statistiques), et l'inventaire des cartes graphiques
(modèle, VRAM, NVENC) — jamais les messages, jamais la voix. Le serveur
archive tout dans `data/diag/`, un fichier JSONL par joueur, borné (~10 Mo,
rotation).

**Les rapports de plantage partent quoi qu'il en soit**, option cochée ou
non — décision de l'admin du groupe : un plantage se corrige avec sa pile
d'appels, pas avec un « ça a planté » de mémoire, et le rapport ne contient
que du technique. Ils existent grâce au dispositif de secours
(`secours.rs`) : le binaire de production n'a pas de console et le moindre
panic le tuait sans un mot, quand un seul raté graphique — pilote
réinitialisé sous un jeu, veille moderne, bascule iGPU/dGPU d'un portable —
suffisait à eframe pour fermer la fenêtre « proprement ». Désormais un
journal sur disque, un panic hook qui y écrit avant l'abort, et la
**relance automatique** après une erreur de rendu (bornée : une erreur qui
revient dès le démarrage épuise son budget et rend la main).

Lecture : un compte **ADMINISTRATOR** a l'onglet « Diagnostics » dans le
panneau d'administration — chaque version y est résumée d'un coup d'œil,
les plantages en tête (« ⚠ N PLANTAGES »), tout se récupère et se copie en
deux clics. Et hors application, le porteur du jeton `data/diag.token`
(généré au premier démarrage, à lire sur la machine du serveur) :

```bash
curl -k -H "x-ki-admin: $(cat diag.token)" https://ton-serveur:8080/diag
```

liste les archives — **classées par version de ki-chat**, l'historique des
bugs de chaque livraison. `/diag/<version>/<fichier>` en renvoie une
(`?tail=65536` pour la fin seulement), et `DELETE /diag/<version>` purge
tout le lot d'une vieille version. `-k` parce que le certificat est
auto-signé — l'épinglage d'empreinte, lui, est fait par les clients
ki-chat, pas par curl.

## Installer le serveur (Docker / Portainer)

Rien à compiler : l'image du serveur est construite et publiée par GitHub
Actions ([`docker.yml`](.github/workflows/docker.yml)) à chaque poussée sur
`main`, pour **amd64 et arm64** — un VPS ordinaire comme un Raspberry Pi
tirent la même étiquette.

```
ghcr.io/redik123/ki-chat-server:latest
```

Trois gestes, dans Portainer : *Stacks* → *Add stack* → *Web editor*, coller
[`deploy/docker-compose.yml`](deploy/docker-compose.yml), ajouter la variable
`KI_TOKEN` (le code d'invitation), déployer. En ligne de commande :

```bash
docker run -d --name ki-chat --restart unless-stopped \
  -e KI_TOKEN=ton_code_secret \
  -p 9987:9987/udp -p 8080:8080/tcp \
  -v ki-chat-data:/data \
  ghcr.io/redik123/ki-chat-server:latest
```

**Le port qui compte est 9987/udp.** Depuis la migration QUIC, l'auth, le
chat, la voix et le partage d'écran y passent tous : sans UDP ouvert de bout
en bout, personne ne se connecte — ce n'est plus « le chat marche, le vocal
non ». Le 8080/tcp ne sert qu'à télécharger les fichiers partagés et à lire
les diagnostics.

### Mises à jour depuis GitHub

La chaîne part du dépôt et se termine sur l'hôte sans intervention :
`git push` → le workflow construit l'image et la publie sur GHCR → le serveur
la récupère. Trois façons de fermer la boucle, au choix :

- **Watchtower**, livré dans le compose : il sonde le registre toutes les cinq
  minutes et recrée le conteneur. Contrepartie assumée — il faut lui prêter la
  socket Docker ;
- **Portainer branché sur le dépôt** (stack de type *Repository* + *GitOps
  updates*), qui ne demande aucune socket ;
- **un webhook Portainer** appelé par le workflow, pour un déploiement
  quelques secondes après le build plutôt qu'au prochain sondage.

Le volume `/data` n'est jamais touché par une mise à jour : comptes,
historique, fichiers partagés, identité du serveur et clé privée TLS lui
survivent. `KI_VERSION=0.1.1` épingle une version si tu préfères décider
toi-même quand bouger.

Le détail — ouverture des ports chez un hébergeur, image privée sur GHCR,
sauvegarde du volume, dépannage — est dans
[`deploy/DEPLOY-DOCKER.md`](deploy/DEPLOY-DOCKER.md).

## Installer (côté joueur)

Télécharger **`ki-chat-setup.exe`** depuis la [dernière
release](https://github.com/Redik123/ki-chat/releases/latest) et le
double-cliquer. C'est tout : ni compilateur, ni redistribuable, ni droits
d'administrateur.

**Rien à installer à côté.** L'exécutable est lié à la bibliothèque C
statiquement : il ne réclame que des DLL livrées avec Windows. Pas de
« Visual C++ Redistributable » à pousser, donc pas de poste où l'application
refuse de démarrer sur un composant manquant. Le workflow de publication le
vérifie à chaque release (`dumpbin -dependents`) plutôt que de l'espérer.

**L'installation vit dans le profil de l'utilisateur**
(`%LOCALAPPDATA%\Programs\ki-chat`), pas dans `Program Files`. Ce n'est pas
un détail : c'est ce qui donne à l'application le droit d'écrire dans son
propre dossier, donc de se mettre à jour toute seule. Posée dans
`Program Files`, chaque mise à jour réclamerait un UAC — autant dire qu'elle
n'aurait jamais lieu.

> Le binaire n'est pas signé (un certificat coûte quelques centaines d'euros
> par an). Au premier lancement, SmartScreen affiche « Windows a protégé
> votre ordinateur » : *Informations complémentaires* → *Exécuter quand
> même*. L'avertissement disparaît de lui-même à mesure que le fichier
> circule.

### Mise à jour automatique

Au démarrage, le client demande à GitHub la dernière release publiée et
compare son étiquette à sa propre version. S'il y a plus récent, il le
**propose** — et ne touche à rien tant que personne n'a accepté. Un refus
vaut pour cette version : on ne redemande qu'à la suivante, pour que « non »
veuille dire non plutôt que « pas cette fois ». Accepté, il télécharge,
remplace son binaire et redémarre seul.

Le remplacement à chaud repose sur une propriété de Windows : on ne peut pas
*supprimer* un exécutable chargé, mais on peut le *renommer*. L'ancien est
donc écarté sous un autre nom, le nouveau prend sa place, et le résidu est
balayé au démarrage suivant — moment où il n'est plus chargé. Un
téléchargement tronqué (connexion coupée) est rejeté sur sa taille au lieu
d'être installé : mieux vaut pas de mise à jour qu'un binaire à moitié écrit.

La vérification part sur un fil séparé et ne retarde pas l'ouverture de la
fenêtre ; sans réseau, elle échoue en silence.

**Signature des mises à jour.** Une application qui remplace son propre
exécutable ne peut pas se contenter de TLS : quiconque obtient le droit de
publier une release — compte compromis, jeton d'action fuité, actif remplacé
après coup — exécuterait du code arbitraire chez tout le monde. Chaque release
porte donc une **signature Ed25519** (`ki-chat.exe.sig`), que le client vérifie
avec une clé publique gravée dans son propre binaire avant de remplacer quoi
que ce soit. La clé privée ne vit que dans le coffre de GitHub.

Le signeur est un exemple du crate client (`cargo run -p ki-client-gui
--example signer`), donc il partage exactement la même version de la
bibliothèque de cryptographie que le code qui vérifie : une divergence entre
signer et vérifier ne se verrait qu'en production, sur les machines des autres.

> La vérification est **armée depuis la 0.1.12** : la clé publique est gravée
> dans le binaire, et une release sans signature, ou signée par une autre clé,
> est refusée. Changer de clé casserait la chaîne — les clients installés
> n'ont aucun moyen d'apprendre la nouvelle. La procédure, et la génération
> d'une clé privée qui ne doit passer par personne d'autre, sont décrites dans
> [`deploy/SIGNATURE.md`](deploy/SIGNATURE.md).

### Publier une version

1. Monter `version` dans le `Cargo.toml` de la racine ;
2. `git tag v0.2.0 && git push --tags`.

Le workflow [`release.yml`](.github/workflows/release.yml) compile, fabrique
l'installeur (Inno Setup) et publie `ki-chat.exe` + `ki-chat-setup.exe` +
`ki-chat.exe.sig` sur la release ; le même tag construit l'image docker du
serveur. Il **refuse de publier si le tag et le `Cargo.toml` divergent** : un
client à jour comparerait alors sa version à une étiquette plus haute et se
croirait perpétuellement en retard, à proposer en boucle une mise à jour déjà
installée.

L'icône de l'application n'est pas un fichier du dépôt : elle est rendue par
[`build.rs`](crates/client-gui/build.rs) aux sept tailles que réclame le shell,
avec le code qui dessine déjà l'icône de fenêtre
([`appicon.rs`](crates/client-gui/src/appicon.rs)). Un seul dessin, rien à
régénérer à la main.

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

### Mesurer

Trente personnes ne se réunissent pas sur commande pour valider un correctif,
et « ça rame quand je joue » ne se reproduit jamais sur la machine de
développement. Trois outils remplacent la divination :

```bash
# N clients virtuels : vraies connexions QUIC, vrais comptes, vraie voix
# chiffrée, à la taille et au rythme réels. Aucun matériel audio requis —
# la charge tourne aussi depuis un conteneur posé à côté du serveur.
cargo run --release -p ki-load -- 127.0.0.1 --clients 30 --invite changeme \
    --secondes 60 --muets 20

# les chemins où il y a du calcul par échantillon, et le coût d'une diffusion
cargo bench -p ki-voice        # mixage de N locuteurs, rééchantillonnage
cargo bench -p ki-protocol     # diffusion d'un roster à N destinataires
```

`--muets` compte : un salon réel est surtout fait d'auditeurs, et ce sont eux
qui font payer le relais. Le bilan sort l'amplification (paquets reçus sur
paquets émis), les pertes montantes que le serveur signale lui-même, et le
volume de contrôle reçu.

Côté client, **⚙ → Aide & diagnostics → Relevé de performance** donne le
coût de l'interface chez
la personne qui se plaint, et se copie comme le journal audio : temps d'une
image, temps du fil de discussion, messages parcourus sur messages chargés,
images par seconde réellement peintes, et trames audio incomplètes (chacune
est un craquement — zéro est la seule bonne valeur). Des quantiles, pas des
moyennes : une moyenne de 3 ms cache une image sur vingt à 40 ms, et c'est
celle-là qui se voit. Le compteur d'allocations par image demande
`--features mesures`, parce qu'une incrémentation atomique par allocation
coûterait sur la ligne de cache que se disputent l'interface, le réseau et
l'audio.

L'application retient serveur/pseudo et les réglages audio entre les
sessions. Le push-to-talk est **global** : la touche fonctionne même quand la
fenêtre n'a pas le focus (poll clavier système via GetAsyncKeyState), donc en
plein jeu. Modes : micro ouvert ou push-to-talk, touche configurable. Deux
**raccourcis globaux** de plus, à régler à côté (aucune touche par défaut) :
couper / rétablir le micro, et **se rendre sourd** — plus rien ne sort de la
carte son, le micro se coupe avec, sortir du vocal rend l'écoute. Un bouton
casque à côté du micro fait la même chose à la souris.

Elle est surveillée sur son **propre fil, à 100 Hz**, et non dans la boucle de
rendu. Deux conséquences. La touche ne rate plus une pression brève — à la
cadence de rendu précédente, vingt fois par seconde, une pression de moins de
cinquante millisecondes passait entre deux images. Et surtout, **la fenêtre
n'a plus besoin de se repeindre pour savoir si l'on parle** : hors vocal, elle
ne repeint plus du tout tant que rien ne bouge, au lieu de reconstruire vingt
fois par seconde un écran identique — y compris réduite, pendant une partie.

En salon vocal, ki-chat empêche aussi la **mise en veille** de la machine :
pour Windows, parler n'est pas être actif, et un portable dont on ne touche
pas le clavier s'endormait en pleine conversation. L'écran garde le droit de
s'éteindre, et hors vocal rien n'est retenu.

### Docteur audio

« Mon micro bugue quand je lance Valorant » est le problème le plus tenace du
projet, et il ne se reproduit jamais sur la machine de celui qui développe.
Windows n'offre par ailleurs **aucune API de « priorité micro »** : quand un
autre logiciel tient la voie de capture — la voix intégrée d'un jeu, la chaîne
d'effets d'un casque, un pilote virtuel — on ne peut pas la lui reprendre. On
peut seulement récupérer vite (ce que font les correctifs précédents) et
**nommer la cause**.

⚙ → Aide & diagnostics → **Docteur audio** fait les deux dernières choses :

- il **détecte les logiciels qui s'interposent** — SteelSeries Sonar et GG,
  Nahimic, Razer Synapse, Logitech G HUB, NVIDIA Broadcast, Voicemeeter,
  Valorant — et donne pour chacun le réglage précis qui rend la main ;
- il **reconnaît un périphérique virtuel en service** (câble VB-Audio,
  Voicemeeter, micro NVIDIA Broadcast). Ce sont des pilotes, pas des
  processus : rien ne les signale, et pourtant parler dans un câble virtuel
  est la cause la plus simple d'un micro muet ;
- il rapporte ce que le moteur a **mesuré** : ouvertures du micro sans un seul
  bloc reçu (la signature d'une voie de capture volée), trames incomplètes
  parties vers la carte son (autant de craquements), et si le moteur natif
  tourne vraiment ou si l'on est retombé sur le moteur de secours ;
- il conseille la **sortie audio robuste** (⚙ → Audio → Sortie) dès qu'une
  trame incomplète est partie : 200 ms de tampon au lieu de 60, pour les
  machines qu'un jeu sature ; et il nomme l'**Atténuation VoIP** de
  Valorant quand « le son du jeu baisse par à-coups » — un micro de bureau
  capte la fuite du casque, la détection vocale du jeu croit qu'on parle et
  baisse le jeu, puis le remonte.

Le tout se copie d'un bouton, comme le journal audio — c'est ce qu'on se fait
envoyer par quelqu'un qui « a le bug » plutôt que de deviner.

> **ki-chat ne touche à aucun réglage système.** Décocher « mode exclusif » à
> la place de quelqu'un demanderait des droits d'administrateur, toucherait
> une configuration qui ne nous appartient pas, et casserait en silence les
> logiciels qui en dépendent — une station audionumérique, un pilote ASIO. Le
> docteur dit quoi regarder ; c'est l'utilisateur qui décide.

À distance, la même chose sans lancer l'interface :
`cargo run -p ki-voice --example sonde-audio`.

**Réglages audio** (⚙ → Audio ; la fenêtre ⚙ se range en onglets : Audio,
Réseau & qualité, Diffusion d'écran, Overlay en jeu, Sons & notifications,
Aide & diagnostics, l'onglet ouvert étant mémorisé) — tout est appliqué à
chaud et mémorisé :
- périphériques d'entrée/sortie (avec actualisation de la liste) ;
- vumètre micro en direct, gain d'entrée 0–200 % ;
- trois modes : micro ouvert, push-to-talk (touche globale), **activation
  vocale** — par **détection de parole neuronale** (Silero VAD, exécuté par
  le même moteur d'inférence que DeepFilterNet, 0,1 ms par bloc de 32 ms :
  un clavier mécanique ou une respiration ne sont plus une voix, jauge de
  probabilité et sensibilité réglable) ou par seuil d'amplitude (jauge verte
  quand la voix déclenche), avec maintien de 400 ms pour ne pas couper les
  fins de mots ;
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
- **annulation d'écho** pour ceux qui écoutent sur haut-parleurs, et
  **sortie audio robuste** (tampon de 200 ms) pour les machines saturées ;
- débit Opus réglable 24–128 kbps, à chaud ;
- **tampon de gigue** : auto (adaptatif) ou fixé de 40 à 160 ms ;
- stats réseau en direct : ping, gigue max, perdus/récupérés FEC/rejetés.

**Résilience réseau** :
- **jitter buffer adaptatif** : la gigue d'arrivée est mesurée en continu
  (EWMA façon RFC 3550) et la latence de tampon s'ajuste seule, de 40 ms sur
  un réseau propre à 160 ms en Wi-Fi chaotique, avec rattrapage anti-dérive ;
- **DRED (redondance neuronale, libopus 1.6.1)** : chaque paquet peut
  re-transmettre jusqu'à ~1 s de passé compressé par réseau de neurones —
  un trou de plusieurs trames est **resynthétisé** depuis n'importe quel
  paquet suivant. Trois positions (⚙ → Réseau & qualité) : désactivé / **Auto**
  (recommandé : s'engage dès 2 % de pertes signalées par le serveur, zéro
  surcoût par beau temps) / toujours (~+32 kbps). Les trames irrécupérables
  passent au **Deep PLC** (masquage de perte neuronal) au lieu de
  l'interpolation classique. libopus 1.6.1 est compilé depuis les sources
  officielles par `crates/ki-opus` (empreinte vérifiée) — l'écosystème Rust
  étant figé sur libopus 1.3.1. ⚠ le format DRED est verrouillé par version :
  tous les clients doivent embarquer le même libopus (garanti par nos builds).
- **récupération FEC réelle** : une trame perdue est reconstruite à partir des
  données de redondance du paquet suivant (et non plus seulement masquée) ;
- **ping vocal en direct** dans la barre du bas, mesuré sur le vrai chemin UDP
  (keepalive horodaté, écho serveur) — vert < 30 ms, orange < 80, rouge au-delà ;
- **vumètres par locuteur** dans la liste des membres pendant qu'ils parlent.
**Rôles** : chaque rôle porte un nom, une **couleur de pseudo**, un **rang** et
un jeu de **permissions** cochables (voir les salons, écrire, rejoindre le
vocal, partager des fichiers, créer des invitations, expulser, bannir,
réinitialiser les mots de passe, gérer les salons, les rôles, le serveur,
consulter le journal, supprimer les messages des autres). Trois rôles existent au départ : `@everyone`
(implicite, jamais attribué — c'est le socle de tout le monde), `Propriétaire`
(le premier compte créé) et `Modérateur`. Onglet **Rôles** du panneau admin
pour les créer, **Membres** pour les attribuer.

Deux règles gouvernent tout, et elles sont **distinctes à dessein** :

- la **permission** dit ce qu'on peut faire ;
- le **rang** dit *sur qui*. On n'agit que sur strictement plus bas que soi,
  et le bit « administrateur » ne contourne **jamais** cette règle — sans
  quoi un second administrateur bannirait le propriétaire.

S'y ajoutent deux gardes contre l'escalade : on n'accorde pas une permission
qu'on ne détient pas soi-même, et l'on ne touche pas à un rôle de son propre
rang ou au-dessus. Sans elles, « gérer les rôles » suffirait à devenir
administrateur en un clic. L'interface ne montre d'ailleurs que les actions
qui aboutiraient : un bouton absent vaut mieux qu'un bouton grisé.

**Salons** : créés à la volée depuis l'onglet **Salons** (textuels ou vocaux),
et **privés** si on les réserve à certains rôles — ils sont alors invisibles
pour les autres, qui reçoivent le message d'un salon inexistant s'ils tentent
d'y entrer. Un salon vocal peut recevoir un **mot de passe éphémère** : il vit
en mémoire, expire au délai choisi et de toute façon au redémarrage du
serveur. Supprimer un salon **archive** son historique (`channel-N.deleted-….jsonl`)
au lieu de l'effacer, et son numéro n'est jamais réattribué — le réutiliser
ferait hériter un nouveau salon des messages de l'ancien.

**Fichiers** : bouton 📎 dans le chat (25 Mo max), upload authentifié
par le jeton de session, les liens sont cliquables dans le chat.

**Modération** — clic droit sur un membre :
- **Expulser** : la personne est déconnectée, mais peut revenir aussitôt.
- **Bannir…** : ouvre une fenêtre avec un **motif** et une **durée** (1 heure,
  1 jour, 7 jours, 30 jours, définitif). Le motif est renvoyé à la personne
  bannie lors de sa prochaine tentative de connexion — sans lui, elle écrit à
  l'admin et il faut traiter la question à la main. Un bannissement à durée se
  lève tout seul : il n'y a aucune tâche de fond, l'expiration est constatée à
  la première tentative de connexion.

**Panneau admin** (bouton ♛ Admin, visible pour les admins), en quatre onglets :
- **Serveur** : nom et logo.
- **Membres** : liste complète avec statut en ligne, motif et auteur des
  bannissements en cours, bouton **Annuler le ban**, réinitialisation de mot de
  passe (un admin ne peut pas modifier un autre admin, mais peut se modifier
  lui-même ; les admins ne sont ni bannissables ni expulsables).
- **Invitations** : génération de codes (`ki-xxxxxxxxxx`) avec un nombre
  d'utilisations au choix — **1, 5, 25 ou illimité**. Un code illimité est un
  lien permanent : il ne s'épuise pas, mais chaque compte qu'il crée est
  consigné au journal, avec l'adresse d'origine. Une étiquette libre (« tournoi
  du samedi ») aide à s'y retrouver. Les codes se révoquent d'un clic et
  restent listés une fois révoqués ou épuisés — c'est l'historique des accès.
  Le code maître `KI_TOKEN` reste valable mais n'a plus besoin de circuler.
- **Journal** : les actions d'administration, de la plus récente à la plus
  ancienne — invitations créées, révoquées et **utilisées**, expulsions,
  bannissements et levées, mots de passe réinitialisés, changements d'identité
  du serveur. Persisté dans `data/audit.jsonl`, une entrée par ligne, lisible
  au `grep` sur le serveur sans y déployer d'outil.

Commandes CLI équivalentes : `/admin`, `/audit`, `/roles`,
`/setroles <pseudo> [id...]`, `/mkchannel <nom> [rôles...]`,
`/rmchannel <id>`, `/lock <id> <mdp> [minutes]`, `/invite`,
`/invite-permanent`, `/revoke <code>`, `/resetpw <pseudo> <mdp>`,
`/ban <pseudo> [minutes] [motif]`, `/unban <pseudo>`, `/kick <id> [motif]`,
`/passwd <ancien> <nouveau>`.

**Effets sonores** : rien n'est embarqué dans le binaire — les fichiers audio
sont affaire de goût, et souvent d'œuvres tierces. Chacun dépose donc les
siens, au format **WAV** (n'importe quelle fréquence, mono ou stéréo : ils
sont convertis en 48 kHz mono au chargement), dans un dossier `sons` placé à
côté de l'exécutable ou dans `%APPDATA%\ki-chat\sons`. Le **nom du fichier**
fait le lien avec l'événement :

| Fichier | Joué quand |
| :--- | :--- |
| `rejoint-vocal.wav` | j'entre dans un salon vocal |
| `quitte-vocal.wav` | j'en sors |
| `arrivee.wav` | quelqu'un entre dans **mon** salon vocal |
| `depart.wav` | quelqu'un en sort |
| `message.wav` | message reçu (jamais pour les siens) |
| `micro-coupe.wav` / `micro-actif.wav` | coupure et réactivation du micro |

Un son absent ne manque à personne : il n'est simplement pas joué. Réglages
dans **⚙ → Sons & notifications** (interrupteur, volume, rechargement à chaud, et
la liste de ce qui a été trouvé). Les sons passent par le volume de sortie
général : baisser le son baisse aussi les notifications.

Pour convertir un MP3 :

```bash
ffmpeg -i source.mp3 -ac 1 -ar 48000 -c:a pcm_s16le arrivee.wav
```

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
> Le chemin n'est volontairement pas `force`é : une variable `CMAKE` déjà
> posée dans l'environnement l'emporte, ce qui laisse l'intégration continue
> — où Visual Studio n'est ni au même endroit ni de la même année — utiliser
> le sien sans qu'un fichier taillé pour une machine précise lui mente.

Variables d'environnement du serveur :

| Variable       | Défaut     | Rôle                          |
|----------------|------------|-------------------------------|
| `KI_TOKEN`     | `changeme` | code d'invitation (création de comptes) |
| `KI_HTTP_PORT` | `8080`     | port HTTPS (partage de fichiers) |
| `KI_UDP_PORT`  | `9987`     | port QUIC (contrôle + voix)   |
| `KI_DATA_DIR`  | `./data`   | persistance (comptes, historique, certificat TLS, identité du serveur) |
| `KI_FILES_MAX_BYTES` | `2 Gio` | plafond global du partage de fichiers ; `0` = illimité |
| `KI_FILES_TTL_DAYS`  | `30`    | âge au-delà duquel un fichier partagé est effacé ; `0` = jamais |

Les deux dernières bornent le disque : sans elles, `data/files/` grandissait
indéfiniment jusqu'à saturer la machine. Une purge passe au démarrage puis
toutes les heures — elle efface d'abord ce qui a dépassé l'âge limite, puis,
si le plafond reste franchi, les fichiers les plus anciens jusqu'à repasser
dessous. Un envoi qui ferait déborder le plafond est refusé avant écriture.
Le défaut n'est **pas** « illimité » à dessein : laisser le réglage de côté
ne doit pas laisser le problème entier.

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
- [x] **M5.1** — Opus 1.6.1 : bindings maison (`ki-opus`, build vérifié depuis les sources officielles), DRED (redondance neuronale ~1 s, mode Auto piloté par les pertes mesurées), Deep PLC (masquage neuronal), OSCE compilé
- [x] **M6** — refonte de l'interface : thème maison (`theme.rs`), jeu d'icônes vectorielles dessinées (`icons.rs` — les polices d'egui ne couvrent pas `●`, `↑`, `✕` : ils sortaient en carrés vides), avatars par pseudo, messages groupés avec séparateurs de journée, barre vocale et panneaux redessinés, icône d'application générée
- [x] **M6.1** — carnet de serveurs (`servers.rs`) : plusieurs serveurs nommés dans un seul client, identifiants (et mot de passe, si demandé) mémorisés par serveur, état et ping mesurés **avant** de se connecter par une poignée de main QUIC de test
- [x] **M6.5** — salons textuels et vocaux distincts : le vocal se rejoint à la demande, plus à la connexion ; liste des connectés au serveur en colonne de droite, occupants affichés sous chaque salon vocal
- [x] **M6.4** — photos de profil : chacun choisit la sienne depuis « Mon compte », le serveur la range dans le compte (`data/users.json`) et la diffuse ; la liste des membres ne porte qu'une empreinte, le client ne réclame que les vignettes qui lui manquent
- [x] **M6.3** — mots de passe mémorisés scellés par le coffre natif (DPAPI sur Windows), derrière une abstraction prête pour le Trousseau macOS/iOS et le Keystore Android ; les anciens mots de passe en clair sont chiffrés au chargement et effacés du fichier
- [x] **M6.2** — identité de serveur : nom + logo persistés côté serveur (`data/server.json`), réglés par les admins et poussés à tous les membres ; vignette PNG 64×64 aux coins arrondis, monogramme coloré à défaut. Côté client, seul un **alias local** est modifiable — le logo ne l'est pas, pour qu'un serveur ne puisse pas en imiter un autre
- [x] **M7** — livraison Windows : exécutable autonome (CRT statique, aucune dépendance à installer), icône et manifeste gravés dans le binaire, installeur Inno Setup sans droits d'administrateur, mise à jour automatique depuis les releases GitHub (proposée, jamais imposée), workflow de publication sur tag
- [x] **M8** — modération et traçabilité : bannissement avec **motif et durée** (levée automatique à l'expiration, constatée à la connexion suivante), annulation d'un ban, expulsion motivée ; invitations à **usages multiples ou permanentes**, étiquetables et révocables, conservées une fois épuisées ; **journal d'audit** (`data/audit.jsonl`) consignant qui est entré par quel lien, et toute action d'administration ; panneau admin réorganisé en onglets. Robustesse au passage : écritures de fichiers atomiques, anti-spam du chat, sauvegardes sorties de la boucle asynchrone
- [x] **M8.1** — corrections : l'indicateur « untel parle » ne s'allumait jamais pour les autres (la diffusion filtrait sur le salon **textuel** alors qu'on lui passait un identifiant de salon **vocal** — les deux jeux d'identifiants étant disjoints, la condition n'était jamais vraie) ; le nom et le logo du serveur n'apparaissaient qu'après qu'un admin les ait ré-enregistrés (le client jetait le champ pourtant présent dès le `Welcome`) ; la fenêtre revenait parfois minuscule en haut à gauche (géométrie restaurée sur un écran secondaire débranché depuis) ; le sélecteur d'image était partagé entre la photo de profil et le logo du serveur
- [x] **M9** — robustesse audio face aux jeux (0.1.6 → 0.1.11) : rebranchement logiciel du casque que Valorant ou Synapse rebrassent, moteur WASAPI natif, périphérique par défaut par le rôle des jeux, redémarrages du moteur rangés en file, docteur audio qui nomme le coupable ; puis (0.1.12) le moteur voix qui survit à la coupure et à la reconnexion, et la signature des mises à jour armée
- [x] **M10** — ce que l'admin voit : dispositif de secours (journal sur disque, panic hook, relance après un raté graphique, anti-veille en vocal), diagnostics partagés classés par version avec leur onglet admin, rapports de plantage qui remontent toujours, annulation d'écho, inventaire des cartes graphiques dans les rapports, sortie audio robuste
- [x] **S1–S2** — partage d'écran (0.1.17 → 0.1.23) : protocole et relais SFU vidéo, diffusion et visionnage, badge à côté du pseudo et réglages de diffusion, réduction d'image, NVENC sans SDK, capture qui ne demande à Windows que ce qu'il sait faire
- [x] **M11** — overlay « qui parle » par-dessus le jeu (0.1.24), désactivé de base (0.1.25)
- [x] **S3, entamé** — le son du jeu dans le stream (0.1.25) ; la vidéo en retard qui ne fait plus la queue devant la voix (0.1.26)
- [ ] **Idées** : débit vidéo qui s'adapte à la connexion la plus courte du salon, synchronisation image/son, stéréo chez le spectateur, statut « en jeu », raccourcis globaux muet/sourdine, clip des 30 dernières secondes, stockage S3 des médias
