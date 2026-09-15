# ki-chat

Serveur de chat privé façon Discord, 100 % Rust, taillé pour le jeu entre amis : chat texte temps réel, vocal ultra-basse latence avec débruitage neuronal, partage d'écran 60 i/s avec le son du jeu isolé, enregistreur de clips rétroactif à la touche, visionneuse photo et vidéo intégrée, bot musique sans publicité et intégration VALORANT poussée — pour ~30 personnes, sur une machine modeste à 5 €/mois.

---

## Sommaire

1. [En un coup d'œil](#en-un-coup-dœil)
2. [Installer & Jouer (côté joueur)](#installer--jouer-côté-joueur)
   - [Windows](#windows)
   - [macOS](#macos)
   - [Raccourcis globaux & Push-to-talk](#raccourcis-globaux--push-to-talk)
   - [Le Docteur audio](#le-docteur-audio)
3. [Héberger son serveur (côté admin)](#héberger-son-serveur-côté-admin)
   - [Déploiement en un clic (Docker / Portainer)](#déploiement-en-un-clic-docker--portainer)
   - [Réseau : le port 9987/udp](#réseau--le-port-9987udp)
   - [Mises à jour automatiques](#mises-à-jour-automatiques)
   - [Variables d'environnement](#variables-denvironnement)
   - [Diagnostics partagés & Rapports de plantage](#diagnostics-partagés--rapports-de-plantage)
4. [Les fonctionnalités en détail](#les-fonctionnalités-en-détail)
   - [Vocal haute fidélité & IA](#vocal-haute-fidélité--ia)
   - [Partage d'écran & Son du jeu](#partage-décran--son-du-jeu)
   - [Clips : les 30 dernières secondes à la touche](#clips--les-30-dernières-secondes-à-la-touche)
   - [Visionneuse média & Téléversement par morceaux](#visionneuse-média--téléversement-par-morceaux)
   - [Intégration VALORANT](#intégration-valorant)
   - [Bot musique de groupe](#bot-musique-de-groupe)
   - [Sécurité, Rôles & Modération](#sécurité-rôles--modération)
5. [Architecture & Développement](#architecture--développement)
   - [Organisation des crates](#organisation-des-crates)
   - [Transport QUIC (TLS 1.3)](#transport-quic-tls-13)
   - [Mesurer la charge (ki-load & benchmarks)](#mesurer-la-charge-ki-load--benchmarks)
   - [Compiler & Publier une version](#compiler--publier-une-version)
6. [Coûts réels](#coûts-réels)
7. [Feuille de route](#feuille-de-route)

---

## En un coup d'œil

- **Vocal ultra-basse latence (~60–80 ms)** : Moteur audio WASAPI natif (sans passer par les couches de mixage Windows destructrices), codecs Opus 1.6.1 avec redondance neuronale **DRED** et masquage de perte **Deep PLC**, débruitage neuronal studio **DeepFilterNet3** en local (~1 ms de calcul), détection de parole **Silero VAD** et annulation d'écho acoustique SpeexDSP.
- **Partage d'écran 60 i/s fluide** : Windows Graphics Capture (matériel, sans injection), encodage matériel **NVENC direct** (sans SDK ni CUDA) ou openh264, flux QUIC chiffré dédié par trame sans blocage de ligne, et **boucle audio WASAPI par processus** qui isole tout le système sauf ki-chat (le son du jeu sans l'écho des copains).
- **L'enregistreur de clips (C1)** : Une touche globale en plein jeu (Alt+F10 par défaut), et les 30 dernières secondes sont enregistrées sur le disque (`Vidéos\ki-chat\`). Encodage direct en mémoire tampon circulaire, son découpé en **pistes séparées** (mix stéréo, son du jeu, micro nettoyé, voix des copains) et galerie de relecture instantanée dans l'application.
- **Visionneuse photo & vidéo intégrée (C0)** : Finies les ouvertures intempestives dans le navigateur web. Les photos et vidéos partagées dans le chat s'ouvrent en grand dans ki-chat (zoom, avance à la seconde, son synchronisé avec l'annulateur d'écho). Les vidéos envoyées sont normalisées par ffmpeg côté serveur en MP4 universel et téléversées par morceaux de 8 Mo (jusqu'à 512 Mo).
- **Overlay en jeu « qui parle »** : Connaître les locuteurs sans quitter la partie. Fenêtre découpée à la forme exacte des pastilles, transparente aux clics, invisible pour les anti-triches (Riot Vanguard, Easy Anti-Cheat) car **rien n'est injecté**.
- **Intégration VALORANT** : Statut en jeu en direct (partie en cours, score, carte, taille d'escouade) lu en lecture seule sur le client Riot local ; fiches de joueurs détaillées avec historique et courbe de RR ; classement du groupe, boutique de skins du jour et **fil de jeu automatique** annonçant les victoires dans le salon textuel.
- **Bot musique de groupe** : Membre virtuel pilotable par une bannière épurée au-dessus du chat. Recherche YouTube/SoundCloud sans publicité, streaming Opus direct par le serveur (aucun fichier temporaire sur disque ni chez le client), playlists partagées, mise en pause automatique en salon vide et reprise après redémarrage.
- **Souveraineté & Respect de la vie privée** : Chiffrement intégral de bout en bout en transit (QUIC TLS 1.3 + XChaCha20-Poly1305 pour la voix et la vidéo), mots de passe hachés en Argon2id, secrets protégés par le coffre natif du système (DPAPI Windows / Trousseau macOS). Zéro pistage, zéro télémétrie commerciale.

---

## Installer & Jouer (côté joueur)

### Windows

1. Télécharger **`ki-chat-setup.exe`** depuis la [dernière version publiée](https://github.com/Redik123/ki-chat/releases/latest).
2. Double-cliquer pour installer.

**Rien d'autre à installer** : L'exécutable est lié statiquement à la bibliothèque C de Windows. Il ne réclame aucun redistribuable externe (« Visual C++ Redistributable ») et ne demande **aucun droit administrateur**.

L'application s'installe dans le profil de l'utilisateur (`%LOCALAPPDATA%\Programs\ki-chat`), ce qui lui permet d'appliquer les mises à jour automatiques sans jamais déclencher d'invite UAC agaçante.

> Le binaire n'étant pas signé par un certificat d'autorité commercial (très coûteux), Windows SmartScreen peut afficher un avertissement au tout premier lancement : cliquer sur *Informations complémentaires* → *Exécuter quand même*. Cet avertissement s'efface à mesure que le binaire circule.

### macOS

L'installation se fait en une commande dans le Terminal :

```bash
curl -fsSL https://raw.githubusercontent.com/Redik123/ki-chat/main/installer/macos/install.sh | sh
```

Le script télécharge la dernière release, déploie `ki-chat.app` dans `~/Applications` (dossier utilisateur sans besoin de mot de passe `sudo`) et ouvre l'application. Relancer cette commande effectue la mise à jour. Pour les adeptes du clic, l'archive classique **`ki-chat-macos.pkg`** est également fournie avec chaque release.

- **Binaire universel** : Compatible nativement puces Apple Silicon (M1/M2/M3/M4) et processeurs Intel (macOS 11 Big Sur minimum).
- **Autorisation Accessibilité** : Pour que le push-to-talk fonctionne lorsque ki-chat est en arrière-plan, activer ki-chat dans *Réglages Système* → *Confidentialité et sécurité* → *Accessibilité*.
- *Note sur macOS* : Le visionnage de stream fonctionne parfaitement avec le son ; la diffusion de son propre écran et l'overlay en jeu sont réservés à Windows pour le moment.

### Raccourcis globaux & Push-to-talk

Surveillés sur un fil système dédié à 100 Hz (`GetAsyncKeystate` / hooks bas niveau), les raccourcis ne ratent jamais une frappe brève, même au cœur d'une partie effrénée :

| Action | Raccourci par défaut | Personnalisable |
| :--- | :--- | :--- |
| **Sauver un clip (30 secondes)** | `Alt + F10` (tenu par Windows : marche au-dessus du jeu) | Oui (⚙ → Clips) |
| **Push-to-talk (Parler)** | Désactivé par défaut | Oui (⚙ → Audio) |
| **Couper / Rétablir le micro** | Non assigné | Oui (⚙ → Audio) |
| **Sourdine casque (Rendre sourd)** | Non assigné | Oui (⚙ → Audio) |

Un mode **activation vocale** épaulé par le réseau neuronal **Silero VAD** permet de jouer micro ouvert sans transmettre les bruits de clavier mécanique, les souffles ou les clics de souris.

### Le Docteur audio

« Mon micro ne marche plus quand je lance mon jeu » est un problème fréquent sous Windows : les moteurs anti-triche ou les suites logicielles pour casques (SteelSeries Sonar, Razer Synapse, Voicemeeter, NVIDIA Broadcast) s'approprient les voies audio en mode exclusif sans prévenir.

Dans **⚙ → Aide & diagnostics → Docteur audio**, ki-chat analyse en temps réel votre configuration :
- Détecte les logiciels et pilotes virtuels qui s'interposent et indique la manipulation exacte pour libérer la voie.
- Relève les trames incomplètes et prévient si la voie de capture a été rendue muette par un tiers.
- Propose l'activation du mode **Sortie audio robuste** (tampon de 200 ms) pour garantir un son cristallin même lorsque le processeur est saturé par un jeu lourd.

---

## Héberger son serveur (côté admin)

### Déploiement en un clic (Docker / Portainer)

Le serveur ne requiert aucune compilation manuelle. Une image Docker multi-architecture (**amd64 & arm64**) est compilée et publiée à chaque mise à jour sur GitHub Packages (GHCR) :

```bash
docker run -d --name ki-chat --restart unless-stopped \
  -e KI_TOKEN=mon_code_invitation_secret \
  -p 9987:9987/udp -p 8080:8080/tcp \
  -v ki-chat-data:/data \
  ghcr.io/redik123/ki-chat-server:latest
```

Pour les utilisateurs de **Portainer**, copiez simplement le fichier [`deploy/docker-compose.yml`](deploy/docker-compose.yml) dans l'éditeur de Stack et définissez votre variable `KI_TOKEN`.

### Réseau : le port 9987/udp

Le protocole repose sur **QUIC** (HTTP/3 sous-jacent avec TLS 1.3) :
- **Port 9987/udp (INDISPENSABLE)** : Tout le trafic applicatif (authentification, chat, présence, salons vocaux SFU, diffusion d'écran en direct) transite sur ce port unique. Si ce port UDP n'est pas redirigé ou ouvert dans votre pare-feu, les clients ne pourront pas se connecter.
- **Port 8080/tcp (HTTPS)** : Utilisé exclusivement pour le téléchargement direct des fichiers/médias partagés et la consultation sécurisée des diagnostics par l'administrateur.

### Mises à jour automatiques

- **Watchtower** (recommandé, inclus dans `docker-compose.yml`) : Vérifie les nouvelles images sur GHCR toutes les 5 minutes et redémarre le conteneur en douceur.
- **Persistance garantie** : Le volume `/data` conserve l'intégralité des comptes (`users.json`), des salons et de l'historique (`channels/`), des certificats TLS auto-signés (`quic-cert.der`), de l'identité du serveur (`server.json`) et du journal d'audit (`audit.jsonl`). Une mise à jour du conteneur ne détruit aucune donnée.

### Variables d'environnement

| Variable | Valeur par défaut | Description |
| :--- | :--- | :--- |
| `KI_TOKEN` | `changeme` | Code d'invitation maître pour la création du premier compte (propriétaire) |
| `KI_UDP_PORT` | `9987` | Port QUIC (contrôle, vocal, vidéo) |
| `KI_HTTP_PORT` | `8080` | Port HTTPS (fichiers, diagnostics) |
| `KI_DATA_DIR` | `./data` | Répertoire de persistance sur disque |
| `KI_FILES_MAX_BYTES` | `2147483648` (2 Gio) | Quota de stockage total des fichiers partagés (purge automatique LRU) |
| `KI_FILES_TTL_DAYS` | `30` | Durée de rétention des fichiers ordinaires (en jours, 0 = illimité) |
| `KI_FILES_MAX_FILE_MB` | `512` | Taille maximale pour un média téléversé par morceaux |
| `KI_CLIPS_MAX_BYTES` | `8589934592` (8 Gio) | Quota des clips partagés (`data/clips/`, purge LRU à part des fichiers) |
| `KI_CLIPS_TTL_DAYS` | `60` | Durée de rétention des clips partagés (en jours, 0 = illimité) |
| `KI_POLICE` | *(DejaVu Sans Bold)* | Police `.ttf` du titre des exports de clips |
| `KI_HENRIK_KEY` | *(vide)* | Clé d'API [HenrikDev](https://docs.henrikdev.xyz) pour l'intégration VALORANT |
| `KI_FFMPEG` / `KI_FFPROBE` | `ffmpeg` / `ffprobe` | Exécutables vidéo pour la normalisation et l'extraction audio |
| `KI_YTDLP` | `data/outils/yt-dlp` (mis à jour chaque jour depuis la release yt-dlp), sinon `yt-dlp` | yt-dlp du bot musique ; posé par l'admin, plus de mise à jour automatique |
| `KI_PUBLIC_URL` | *(l'adresse par laquelle le client parle au serveur)* | Adresse publique (`https://hote:port`) pour les liens que suit un téléphone (QR code de l'atelier) |

### Diagnostics partagés & Rapports de plantage

En cochant « Partager mes diagnostics » dans les paramètres de l'application, les joueurs transmettent toutes les 10 minutes leur journal technique (modèle GPU, état des pilotes, statistiques réseau et audio — **à l'exclusion stricte de tout message ou donnée vocale**).

Les administrateurs peuvent consulter et analyser les rapports d'erreurs et de plantages directement depuis l'onglet **Diagnostics** du panneau d'administration, ou via l'API protégée par jeton :

```bash
curl -k -H "x-ki-admin: $(cat data/diag.token)" https://ton-serveur:8080/diag
```

---

## Les fonctionnalités en détail

### Vocal haute fidélité & IA

- **Pipeline audio WASAPI natif** : Conçu spécialement pour contourner les latences de mixage génériques de Windows (`eConsole` pour cibler le périphérique de jeu).
- **Opus 1.6.1 de pointe** : Compilé avec les extensions de pointe issues des sources officielles via [`crates/ki-opus`](crates/ki-opus) :
  - **DRED (Deep Redundancy)** : En cas de perte de paquets, jusqu'à 1 seconde de voix passée est resynthétisée par réseau neuronal.
  - **Deep PLC** : Masquage neuronal des trames perdues lorsqu'aucune redondance n'est disponible.
- **Réduction de bruit DeepFilterNet3** : Réseau de neurones de qualité studio exécuté en local via le moteur d'inférence Tract (~1 ms de calcul CPU par trame de 20 ms), éliminant bruits de fond, climatiseurs et frappes de touches sans dénaturer le timbre de la voix.
- **Interpolation cubique de Hermite** : Le rééchantillonnage de 44,1 kHz à 48 kHz réduit l'erreur harmonique d'un facteur 53 par rapport à une interpolation linéaire classique, prévenant tout micro-craquement.
- **Réseau résilient & DSCP EF** : Paquets vocaux marqués `Expedited Forwarding` pour être priorisés par les routeurs DiffServ, couplés à un tampon de gigue adaptatif oscillant entre 40 et 160 ms.

### Partage d'écran & Son du jeu

- **Windows Graphics Capture (WGC)** : Capture matérielle au niveau de l'OS sans accrochage Direct3D ni injection de DLL dans les processus de jeu.
- **NVENC sans SDK tiers** : Chargement dynamique direct de `nvEncodeAPI64.dll` présent dans les pilotes NVIDIA modernes (API 12.0+) ; repli transparent sur l'encodeur logiciel openh264 en cas de matériel non supporté.
- **Boucle WASAPI « tout sauf ki-chat »** : La capture audio du stream intercepte les sons de tous les processus Windows à l'exception de l'exécutable de ki-chat lui-même. Les spectateurs entendent le jeu et la musique du diffuseur, mais n'entendent jamais leur propre écho en retour.
- **Chiffrement de bout en bout** : Chaque flux vidéo est chiffré par le diffuseur en XChaCha20-Poly1305 avec une clé éphémère distribuée aux seuls spectateurs autorisés. Le serveur SFU relaie les trames sans avoir la capacité de les déchiffrer.

### Clips : les 30 dernières secondes à la touche

*(Livré avec le jalon C1 en version 0.1.35)*

Ne laissez plus passer un tir incroyable ou un moment mémorable :

```
[ Jeu / Écran ] ──► Capture WGC ──► NVENC (GOP 1s) ──► Tampon circulaire RAM (30s)
[ Son du jeu ]  ──► WASAPI Loopback ───────────────► ┌── Alt+F10 ────────────────┐
[ Micro traité] ──► Filtres IA / Opus ─────────────► │ Écriture MP4 multi-pistes │
[ Voix salon ]  ──► Moteur vocal ──────────────────► └──► Vidéos\ki-chat\ ───────┘
```

1. **Tampon mémoire léger** : L'enregistreur conserve les N dernières secondes en mémoire vive (réglable de 15 à 120 secondes, 30 s par défaut). Rien n'est écrit sur le disque tant que la touche n'a pas été pressée.
2. **Pistes audio indépendantes** : Le fichier `.mp4` généré par le Sink Writer de Media Foundation contient **quatre pistes audio distinctes** :
   - *Piste 1* : Mix stéréo global équilibré (compatible avec tous les lecteurs multimédias du commerce et les réseaux sociaux).
   - *Piste 2* : Le son du jeu pur (système sans ki-chat).
   - *Piste 3* : Votre voix filtrée et débruitée.
   - *Piste 4* : Les voix de vos coéquipiers dans le salon.
3. **Galerie intégrée** : Retrouvez l'ensemble de vos clips dans l'onglet dédié de l'application, avec lecture immédiate, vignettes générées et raccourci pour ouvrir le dossier local dans l'Explorateur Windows.
4. **Une touche qui passe au-dessus du jeu** : la combinaison est tenue par Windows (`RegisterHotKey`, le mécanisme d'Alt+Tab), pas lue par sondage — elle marche en plein écran, sous VALORANT et son anti-triche, sans lancer ki-chat en administrateur. Ctrl ou Maj tenus en plus (accroupi, en marche) ne la bloquent pas.
5. **Partager dans un salon (C2, 0.1.37)** : clic droit sur un clip → le salon, une légende, et « avec les voix des copains » ou sans. Le clip monte par morceaux dans un stock à part (`data/clips/`, quota et durée de vie propres), le serveur en fabrique une copie **à une seule piste son** — le mélange, ou un mélange refait sans les copains ; leurs voix ne sortent jamais du dossier du clip — et poste le message en votre nom. Chez chacun, la carte vidéo, puis la visionneuse.
6. **L'atelier (C3)** : « Modifier dans l'atelier… » — la coupe entre deux poignées sur une bande de vignettes, le format téléphone 9:16 en quatre mises en page (recadré, avec une position de fin pour suivre l'action ; resserré, presque tout, un peu déformé ; fond flou ; zoom), un titre, un curseur par piste, la cadence. Le serveur fabrique la vidéo d'après cette recette (jamais un filtre venu du client), puis : **Enregistrer sous…**, **Partager dans un salon**, ou **Envoyer sur le téléphone** par un QR code — un lien à jeton valable une heure, à scanner avec l'appareil photo, et la vidéo part sur TikTok ou Instagram depuis la feuille de partage du téléphone.

### Visionneuse média & Téléversement par morceaux

*(Livré avec le jalon C0 en version 0.1.35)*

- **Affichage grand écran** : Zoom fluide à la molette, déplacement panoramique, navigation chronologique d'un média à l'autre dans le salon (touches `←` et `→`), bouton « Copier » et « Enregistrer sous ».
- **Support étendu** : Photos, vidéos MP4/WebM/MKV, GIF et images WebP animées.
- **Normalisation serveur transparente** : Les vidéos brutes issues de téléphones (formats verticaux HEVC d'iPhone, orientations EXIF) sont converties en tâche de fond par ffmpeg sur le serveur en H.264/AAC avec poster JPEG et méta-données.
- **Téléversement par morceaux (Chunking)** : Les fichiers de plus de 25 Mo sont automatiquement découpés en blocs de 8 Mo (jusqu'à 512 Mo) avec reprise sur erreur, s'affranchissant des limites de requêtes des routeurs et reverse proxies.

### Intégration VALORANT

Chantier conçu dans le respect strict des préconisations de Riot Games ([`PLAN-VALORANT.md`](PLAN-VALORANT.md)) :
- **Statut en direct** : Détection locale en lecture seule de la présence de votre propre jeu (sans injection ni scrutation de la mémoire). Affichage du mode, du score et de l'agent joué.
- **Fiches de carrière HenrikDev** : Visualisation du rang compétitif, du pic de saison, du ratio V/D et de l'historique des parties récentes via l'API HenrikDev hébergée sur le serveur.
- **Fil de match automatique** : Dès qu'une partie classée se termine, le serveur poste un résumé détaillé dans le salon textuel choisi (victoire/défaite, score, KDA individuel et gain/perte de RR). Si plusieurs membres du groupe étaient dans la même partie, leurs statistiques sont regroupées dans une seule annonce.
- **Le récap de la semaine** : Le dimanche soir, le fil de jeu poste le bilan de la semaine écoulée — une ligne par membre qui a joué, du plus grand gain de RR au plus petit, avec son rang, ses matchs, victoires et défaites et son meilleur match.
- **Boutique du jour** : Consultation sécurisée de vos 4 skins quotidiens en boutique directement depuis l'application ki-chat (lecture locale protégée de vos jetons Riot).

### Bot musique de groupe

Membre virtuel autonome pour animer vos sessions de jeu ([`PLAN-MUSIQUE.md`](PLAN-MUSIQUE.md)) :
- **Bannière rétractable** : Placée au-dessus du fil de discussion, elle affiche le titre en cours, la pochette, le temps restant et une barre de progression cliquable.
- **Recherche intégrée** : Saisissez un titre ou collez un lien YouTube / SoundCloud pour garnir la file de lecture partagée.
- **Playlists du groupe** : Sauvegardez vos files d'écoute préférées et marquez vos morceaux d'une étoile pour les ajouter aux « Favoris ».
- **Zéro fardeau client** : L'extraction et le réencodage Opus sont gérés à 100 % sur le serveur. Les clients reçoivent un flux audio chiffré similaire à la voix d'un utilisateur ordinaire, réglable individuellement en volume.

### Sécurité, Rôles & Modération

- **Authentification forte** : Mots de passe salés et hachés via **Argon2id**. Protection contre les attaques par force brute avec délais exponentiels calculés *avant* tout calcul lourd de hachage.
- **Hiérarchie par rangs stricts** : Une permission définit *ce que* l'on peut faire, le rang numérique définit *sur qui*. Un administrateur ne peut jamais modérer un autre administrateur de rang supérieur ou égal.
- **Invitations auditables** : Génération de codes d'accès à usage unique, borné ou permanent. Chaque compte créé porte la trace du lien utilisé dans le journal d'audit (`audit.jsonl`).
- **Écritures atomiques** : Toutes les modifications d'état (utilisateurs, canaux, permissions) sont écrites sur un fichier temporaire puis renommées de manière atomique (protection totale contre les corruptions en cas de coupure brutale du serveur).

---

## Architecture & Développement

### Organisation des crates

```
crates/
  protocol/     Types partagés : messages de contrôle JSON, paquets voix,
                en-têtes vidéo (KF) et audio jeu (KA), bornes de sécurité
  server/       ki-server : serveur QUIC (contrôle, SFU voix, SFU vidéo),
                serveur HTTPS (fichiers, morceaux d'upload, diagnostics),
                normalisation ffmpeg, moteur bot musique
  voice/        Moteur audio client : backend WASAPI natif (cpal en secours),
                gestionnaire Opus, jitter buffer adaptatif, annulation d'écho Speex,
                docteur audio, isolation du son du jeu
  video/        Capture d'écran WGC, réduction d'image, encodeurs NVENC et openh264
  media/        ki-media : couche Media Foundation Windows. Décodage Source Reader
                (NV12/PCM) et écriture MP4 par Sink Writer (clips multi-pistes)
  ki-opus/      Bindings et compilation de libopus 1.6.1 officielle (DRED, Deep PLC)
  ki-aec/       Annulation d'écho acoustique SpeexDSP MDF
  client-quic/  Gestion de la connexion cliente QUIC (contrôle fiable + datagrammes)
  client-cli/   Client léger en ligne de commande (chat et vocal)
  client-gui/   ki-chat : application graphique egui/eframe complète
  load/         ki-load : outil de test de charge simulant N clients virtuels réalistes
```

### Transport QUIC (TLS 1.3)

Le protocole repose sur une unique connexion QUIC par client :
- **Flux bidirectionnel fiable** : Transit des événements de contrôle (connexion, chat JSON, changements de rôles, présence).
- **Datagrammes non fiables chiffrés** : Voix temps réel (paquets Opus chiffrés en XChaCha20-Poly1305).
- **Flux unidirectionnels par trame vidéo** : Évite le blocage de tête de ligne entre images successives du partage d'écran.
- **Migration de connexion** : Basculez d'une connexion Wi-Fi à un câble Ethernet (ou partage 4G/5G) sans déconnexion ni coupure vocale.

### Mesurer la charge (ki-load & benchmarks)

Pour valider le comportement du serveur sans mobiliser 30 personnes réelles :

```bash
# Simuler 30 clients virtuels avec connexions réelles, authentification et voix chiffrée
cargo run --release -p ki-load -- 127.0.0.1 --clients 30 --invite changeme --secondes 60 --muets 20

# Benchmarks des chemins critiques audio et protocole
cargo bench -p ki-voice
cargo bench -p ki-protocol
```

### Compiler & Publier une version

**Prérequis de développement** :
- Rust stable (édition 2021).
- Sous Windows : Visual Studio C++ Build Tools et CMake.
- Outils optionnels : `nasm` pour optimiser l'encodage openh264.

```bash
# Lancer le serveur localement en développement
cargo run -p ki-server

# Lancer l'application de bureau
cargo run -p ki-client-gui
```

**Procédure de publication (Release)** :
1. Incrémenter `version = "0.x.y"` dans le `Cargo.toml` racine et exécuter `cargo check` pour aligner `Cargo.lock`.
2. Créer le tag Git correspondant et le pousser :
   ```bash
   git tag v0.x.y
   git push origin main
   git push origin v0.x.y
   ```
3. Le workflow GitHub Actions [`release.yml`](.github/workflows/release.yml) valide les tests, compile les binaires Windows et macOS, signe cryptographiquement les exécutables en Ed25519 et publie la release GitHub.

---

## Coûts réels

**Licences logicielles : 0 €**  
L'intégralité du code et des dépendances utilisées (Rust, libopus, egui, Quinn, Media Foundation) est libre et gratuite (MIT / Apache-2.0 / BSD).

**Budget d'hébergement pour un groupe de ~30 joueurs** :
- **VPS 2 vCPU / 4 Go de RAM** (Hetzner, OVHcloud, Scaleway) : **~5 à 8 € / mois**. Le serveur agissant en tant que relais SFU sans décompresser la voix ni réencoder la vidéo à la volée, la charge processeur reste minime.
- **Nom de domaine** : ~10 € / an (facultatif, une adresse IP brute suffit amplement grâce aux certificats TLS auto-signés épinglés).
- **Hébergement à domicile** : 0 € / mois sur une box fibre (redirection du port 9987/udp).

---

## Feuille de route

### Jalons livrés

- [x] **M0–M2** — Fondations : protocole QUIC, relais vocal SFU, application graphique egui, salons, push-to-talk.
- [x] **M3–M4** — Sécurité & Confort : comptes chiffrés Argon2id, voix chiffrée, débruitage RNNoise, partage de fichiers.
- [x] **M4.5–M4.10** — Rôles, invitations traçables, égaliseur de volume par membre, limiteur doux, vumètres par locuteur, table de routage précalculée et débit adaptatif.
- [x] **M5–M5.1** — Migration QUIC complète (TLS 1.3 natif) et adoption de libopus 1.6.1 avec DRED & Deep PLC.
- [x] **M6–M6.5** — Refonte visuelle complète, carnet de multi-serveurs avec ping préalable, photos de profil synchronisées, mots de passe protégés par DPAPI/Trousseau.
- [x] **M7–M8** — Packaging Windows autonome sans dépendance C++, mise à jour automatique signée Ed25519, journal d'audit immuable.
- [x] **M9–M10** — Robustesse face aux jeux : gestionnaire WASAPI natif, Docteur audio, diagnostics partagés et reprise automatique après crash de pilote.
- [x] **S1–S3** — Partage d'écran 60 fps : capture matérielle WGC, NVENC sans SDK, et isolation de la boucle audio du jeu.
- [x] **M11–M12** — Overlay « qui parle » transparent sans injection ; support et paquet universel macOS.
- [x] **V1–V4 (0.1.31 à 0.1.33)** — Intégration VALORANT complète : statut en jeu en temps réel, fiches de carrière HenrikDev, fil de salon automatique et boutique quotidienne.
- [x] **M1–M3 (0.1.34)** — Bot musique haute fidélité : streaming direct sans pub, recherche YouTube/SoundCloud, playlists partagées et mémorisation d'état.
- [x] **C0 (0.1.35)** — Visionneuse photo & vidéo intégrée, décodeur Media Foundation natif, normalisation serveur ffmpeg et téléversement par morceaux de 8 Mo.
- [x] **C1 (0.1.35)** — Enregistreur de clips rétroactif (30 secondes à la touche Alt+F10), pistes audio séparées en MP4 et galerie de clips locale.
- [x] **C2 (0.1.37)** — Partage de clips dans les salons textuels : stock à part sur le serveur, une seule piste son (avec ou sans les voix des copains), message au nom du membre, carte vidéo — et la touche des clips tenue par Windows, qui passe au-dessus du jeu.
- [x] **C3 (0.1.38)** — Atelier de coupe et de recadrage au format téléphone (9:16, quatre mises en page, titre, mixage), export ffmpeg côté serveur d'après une recette validée, et envoi sur le téléphone par QR code.

### Perspectives & Prochains jalons

- [ ] **C4** — Pipeline de capture vidéo tout-GPU (zéro copie mémoire centrale), encodeur AMD/Intel, une seule capture pour la diffusion et les clips.
- [ ] **Général** — Résolution et cadence comme crans du débit adaptatif (le débit s'adapte déjà au spectateur qui ne suit pas, et le son du jeu lui arrive en stéréo).
