# Plan — les clips et la visionneuse de ki-chat

Une touche en jeu, et les trente dernières secondes sont sauvées — comme la
relecture instantanée de NVIDIA, mais dans ki-chat : une galerie, un partage
dans un salon en un clic, un petit atelier pour couper le clip et le mettre
au format téléphone (TikTok, Instagram), et le fichier sur le téléphone sans
câble. Et, en préalable, une visionneuse : les photos et les vidéos
partagées s'ouvrent dans ki-chat, plus dans le navigateur.

Écrit le 2026-09-14, avant la première ligne de code, comme les plans
VALORANT et musique. Ce qu'on a vérifié (dans le code, dans les sources
des crates, dans leur documentation) est marqué **vérifié** ; le reste est
à vérifier au jalon qui le concerne. Les questions posées à drion sont à
la fin.

## Ce qu'on veut

- **Une touche, trente secondes.** L'enregistreur tourne en fond pendant
  qu'on joue, garde les N dernières secondes en mémoire, et les écrit dans
  un fichier quand on appuie. Léger : le jeu ne doit pas le sentir.
- **Une galerie dans ki-chat** : ses clips, une vignette, la durée ; lire,
  partager dans un salon, modifier, supprimer.
- **Un atelier** : couper le début et la fin, passer au **format
  téléphone** (9:16) avec un recadrage qu'on déplace, un fond flou ou un
  zoom, régler le son (jeu, micro, voix des copains), poser un titre.
- **Sur le téléphone sans câble** : un QR code dans ki-chat, le téléphone
  télécharge, et le partage TikTok/Instagram se fait depuis le téléphone.
- **Une visionneuse photo et vidéo** intégrée : une image ou une vidéo
  partagée dans le chat s'ouvre en grand dans ki-chat, avec lecture,
  avance, volume — et « enregistrer sous » si on veut le fichier.

## Ce que ki-chat a déjà (**vérifié** dans le code)

- **La capture d'écran et de fenêtre** (`crates/video`, Windows.Graphics.
  Capture via windows-capture 2.0.1) : c'est celle du partage d'écran, elle
  marche déjà avec VALORANT chez les copains, et **rien n'est injecté dans
  le jeu** — l'anti-triche n'a rien à y redire (même principe que
  l'overlay « qui parle »). Le crate donne aussi la **texture GPU brute**
  de chaque trame (`Frame::as_raw_texture`, `device`) : de quoi, plus tard,
  encoder sans jamais repasser par le processeur.
- **NVENC** (`nvenc.rs`), entrée texture NV12, secours logiciel après deux
  refus ; **openh264** pour l'encodage logiciel et le **décodage portable**
  (`ViewerDecoder`, celui des spectateurs, y compris sur macOS) ; le
  réducteur d'image ; un GOP de deux secondes des deux côtés ; des trames
  horodatées (`EncodedFrame { data, idr, pts_us, width, height }`).
- **Le son du jeu** (`crates/voice/jeu.rs`) : la boucle WASAPI « tout le
  système sauf ki-chat » (Windows 10 2004+), en float 48 kHz stéréo — les
  voix des copains n'y sont donc **pas**. Le moteur vocal a une file
  d'effets sonores mixée *avant* le volume général et le limiteur : une
  vidéo lue par ce chemin est vue par l'annulateur d'écho, et les copains
  ne l'entendent pas revenir par le micro.
- **Une touche globale** (`ptt.rs`, device_query) : le push-to-talk est
  détecté même quand la fenêtre n'a pas le focus ; le même sondage sait
  voir une combinaison.
- **Le partage de fichiers** (`server/files.rs`) : upload HTTP d'un bloc
  (**25 Mo maximum, « aligné sur la limite du routeur »**), stock borné
  (2 Gio, 30 jours), téléchargement d'un fichier entier sans reprise
  partielle. Côté client (`images.rs`), les images s'affichent sous le
  message, téléchargées **seulement depuis notre serveur** avec le client
  HTTP épinglé sur son empreinte ; un clic les ouvre… dans le navigateur.
  Une vidéo n'est qu'un lien.
- **Le serveur a ffmpeg** (image Docker, build BtbN **gpl**, donc avec
  x264), yt-dlp et deno. Son certificat est **auto-signé** (le même que
  QUIC) : un navigateur avertit une fois.
- **L'interface** : eframe 0.32 sur glow ; `image` (png, jpeg), `rfd`
  (dialogues), `arboard` (presse-papiers), `rayon` déjà dans l'arbre ;
  un exécutable de 40 Mo, installeur Inno Setup.

## Ce qu'on a trouvé

Le tour des projets libres qui pourraient servir, et ce qu'on en fait.

| Projet | Ce que c'est | Ce qu'on en fait |
|---|---|---|
| **windows-capture 2.0.1** (déjà là) | Capture WGC ; contient aussi un encodeur MP4 bâti sur `MediaTranscoder` de Windows, accélération matérielle activée (**vérifié** dans ses sources) — donc H.264 par la carte, **quelle que soit la marque** | La capture, la texture GPU (chemin tout-GPU en C4), et la preuve qu'un encodeur AMD/Intel s'obtient de Windows sans rien embarquer |
| **Media Foundation** (Windows, crate `windows`, déjà là) | L'API média de Windows : *Source Reader* (démultiplexe et décode MP4 H.264/AAC, matériel ou logiciel), *Sink Writer* (écrit un MP4, encode l'AAC et, si on veut, le H.264 par l'encodeur du constructeur) | **Notre décodeur** (visionneuse, atelier) et **notre écrivain de MP4** (clips). Rien à livrer : c'est dans chaque Windows |
| **openh264 0.9** (déjà là) | Décodeur H.264 logiciel, portable | Le chemin macOS de la visionneuse (C4) |
| **symphonia 0.6.1** (MPL-2.0) | Décodage audio 100 % Rust ; AAC et MP4 derrière les drapeaux `aac` et `isomp4` (**vérifié**, docs.rs) | Le son du chemin portable (C4) |
| **mp4 0.14** (MIT) | Lecture/écriture de MP4 en Rust : H.264, HEVC, VP9, AAC — pas d'Opus (**vérifié**, docs.rs) | Plan B de l'écrivain de MP4 si le Sink Writer se montre capricieux ; démultiplexeur du chemin portable |
| **ffmpeg** (déjà sur le serveur) | Le couteau suisse : conversion, vignette, découpe, recadrage, flou, titre, mixage | **Toute la fabrication côté serveur** : normalisation des vidéos partagées, posters, export téléphone |
| **ffmpeg-sidecar 2.5.2** (MIT) | Pilote un binaire ffmpeg depuis Rust, sait le télécharger, rend les images décodées | **Écarté côté client** : 100 Mo de binaire à livrer (l'installeur passerait de 40 à 140 Mo), et un téléchargement d'exécutable au premier lancement n'est pas notre style |
| **rerun (`re_video`)** | Le lecteur vidéo egui le plus abouti : H.264/HEVC **par un ffmpeg externe** exigé sur la machine, AV1 par dav1d, **pas d'audio**, « volontairement pas de ffmpeg embarqué, pour des raisons de licence » (**vérifié**, docs rerun) | La leçon : l'écosystème egui n'a pas de lecteur prêt à l'emploi ; on prend l'API native de Windows, comme on l'a fait pour l'audio |
| **Cap** (cap.so) | Enregistreur d'écran en Rust/Tauri avec atelier (zoom, fonds, coupe, sous-titres, export) ; **AGPLv3** sauf les crates `scap-*` (MIT) (**vérifié**) ; l'atelier est en TypeScript | Inspiration pour l'atelier (les fonds, le zoom animé) ; rien à réutiliser tel quel |
| **OBS Studio** (GPL, C) | Le *replay buffer* de référence : trames encodées gardées en mémoire, coupe à la trame clé | Le dessin de notre tampon circulaire |
| **GStreamer** (gstreamer-rs) | Pipeline média complet, matériel compris | Écarté : un runtime de 100 Mo à installer chez chacun |
| **egui-video** | Lecteur egui sur ffmpeg-next (bibliothèques ffmpeg à lier, vcpkg en CI) | Écarté |
| **MediaPipe AutoFlip** | Recadrage 16:9 → 9:16 automatique par suivi du sujet | Écarté (lourd) : recadrage manuel avec position de début et de fin |
| **qrcode 0.14** (100 % Rust) | Génère un QR code | Le lien vers le téléphone |
| Kdenlive, Shotcut, Olive | Éditeurs vidéo C++ | Inspiration d'interface seulement |

Ce qu'il faut retenir : **rien à télécharger ni à embarquer côté client**.
Le décodage et l'écriture de MP4 viennent de Windows, l'encodage de NVENC
(déjà là), la fabrication lourde du serveur (ffmpeg, déjà là).

## Lignes rouges

- **Un clip ne quitte jamais la machine sans « Partager ».** L'enregistreur
  n'envoie rien, jamais ; la galerie est locale ; le partage est un geste.
- **L'enregistreur est éteint par défaut et visible quand il tourne**
  (point rouge dans la barre) ; il capture **la fenêtre du jeu** quand il
  la reconnaît, pas l'écran entier avec Discord et le navigateur derrière.
- **Les voix des copains** n'entrent dans un clip que si l'option est
  cochée ; le réglage se voit dans la galerie (« avec les voix »).
- **Les diagnostics ne contiennent ni image ni son ni clip** — comme
  aujourd'hui pour les messages et l'audio.
- **Le serveur ne fait tourner que ce qu'il construit lui-même** : le
  client envoie une *recette* structurée (bornes, cadre, niveaux, titre),
  jamais une ligne de commande ni un filtre ffmpeg libre ; le serveur
  valide chaque champ et compose les arguments.
- **Le lien pour le téléphone** est un jeton aléatoire de 128 bits, valable
  une heure, qui ne sert qu'un seul fichier ; il n'ouvre rien d'autre.
- **Le partage de clips obéit à la permission « Partager des fichiers »**,
  et aux quotas du serveur.
- **Rien d'injecté dans le jeu** : capture par le compositeur (WGC),
  touche par sondage, retour par un son et par l'overlay existant.

## Architecture

### L'enregistreur (relecture instantanée)

Un fil « clips » côté client, calqué sur la boucle streamer :

1. **Capture** : la même `start_capture` que le partage d'écran, à la
   cadence choisie (60 ou 30), sur la **fenêtre du jeu** quand on la
   connaît (VALORANT tourne : on cherche `VALORANT-Win64-Shipping.exe`
   dans `list_windows()`), sinon l'écran choisi. Diffuser et enregistrer
   en même temps ouvre deux captures de la même source ; WGC l'accepte, on
   mutualisera plus tard si ça se voit.
2. **Encodage** : NVENC avec un **profil « clip »** distinct du profil
   « diffusion » : GOP d'**une seconde** (la coupe se fait à la trame clé,
   le clip fait donc au plus une seconde de moins que demandé), débit
   12 Mbit/s « équilibré » ou 20 Mbit/s « qualité », profil High, pas de
   trame B (pas de réordonnancement, pas de DTS à calculer). Pas de
   logiciel à 1080p60 : ce serait un cœur entier pendant qu'on joue. Sans
   NVENC, l'enregistreur propose 720p30 logiciel ou refuse, et le dit.
3. **Le tampon** : les trames encodées dans un `VecDeque` avec leur
   horodatage et leur drapeau IDR ; on jette par l'avant tout ce qui
   dépasse la durée, **en s'arrêtant sur une trame clé** (le tampon
   commence toujours par une IDR). En parallèle, les pistes audio en PCM
   float 48 kHz dans des anneaux de même durée :
   - **jeu** : la boucle « tout sauf ki-chat » (existe) ;
   - **micro** : un robinet sur le micro traité du moteur (après
     suppression de bruit) ;
   - **vocal** : un robinet sur le mélange des copains, avant le volume
     général.
   Chaque piste porte l'horodatage de son premier échantillon sur
   l'horloge commune (`origine`, la même que la vidéo).
4. **La touche** : le sondage global existant, déclenchement au front
   (pas de répétition tant qu'on tient), combinaison possible (Alt+F10).
   À l'appui : copie du tampon (quelques dizaines de Mo, instantané),
   écriture sur un fil à part, **son de confirmation** par le moteur
   d'effets, ligne dans l'overlay « qui parle » (« Clip enregistré ») quand
   il est visible, et une pastille dans ki-chat.
5. **Sécurité de marche** : marqueur `clips.en-cours` sur le disque (comme
   `diffusion.en-cours`) — après un plantage, ki-chat propose de laisser
   l'enregistreur éteint ; deux refus NVENC → arrêt propre et message ;
   moins de 500 Mo libres → pas d'écriture, message.

### Le fichier du clip

Un **MP4 standard**, lisible partout (Explorateur, VLC, téléphone) :
H.264 tel qu'encodé (pas de réencodage) + une piste AAC par source audio
(jeu, micro, vocal), 160 kbit/s chacune.

L'écriture passe par le **Sink Writer de Media Foundation** : on déclare
un flux vidéo H.264 dont le type d'entrée *est* le type de sortie (pas
d'encodeur inséré : les trames passent telles quelles), avec SPS/PPS dans
`MF_MT_MPEG_SEQUENCE_HEADER` (NVENC les donne, ou on les lit dans la
première IDR) ; les flux audio entrent en PCM float et sortent en AAC —
c'est le writer qui encode. Un fichier de 30 s s'écrit en moins d'une
seconde. **À vérifier au C1** sur un prototype d'une journée ; plan B :
muxer avec le crate `mp4` et encoder l'AAC par la transformée AAC de
Media Foundation directement.

Nom : `2026-09-14 21h03 VALORANT.mp4` dans `Vidéos\ki-chat\` ; une
vignette JPEG dans le cache de ki-chat ; un petit `.json` à côté (pistes,
source, durée, réglages) pour la galerie.

### La visionneuse et le décodeur

Un crate `ki-media` (ou un module de `ki-video`) avec un trait `Lecteur` :
ouvrir un fichier, connaître durée/dimensions/pistes, **chercher** une
position, **tirer** la prochaine image (RGBA) et le prochain bloc audio.

- **Windows : Media Foundation Source Reader**, sans gestionnaire D3D pour
  commencer (décodage logiciel de Microsoft, rapide et sans surprise de
  pilote), sortie vidéo NV12 → RGBA par notre conversion SIMD, sortie
  audio PCM float 48 kHz stéréo (le reader insère le rééchantillonneur).
  Le DXVA (décodage par la carte) viendra si un portable en a besoin.
- **Portable (C4)** : `mp4` (démultiplexage) + openh264 (déjà là) +
  symphonia (AAC). Pour que ce chemin lise tout ce que le serveur produit,
  la normalisation encode **sans trame B** (openh264 ne les décode pas).

Le fil du lecteur ressemble au fil décodeur du spectateur : il décode en
avance dans un petit anneau d'images (2-3), pousse le son dans une **file
« médias » du moteur vocal** (à côté des effets sonores, avec son propre
volume, mixée avant le volume général et le limiteur), et réveille
l'interface (`request_repaint`). **L'horloge, c'est le son** : une image
s'affiche quand son horodatage est atteint par les échantillons consommés ;
sans piste audio, l'horloge murale.

Les fichiers du serveur sont **téléchargés dans un cache disque** avant
lecture (`%LOCALAPPDATA%\ki-chat\cache\medias\`, 1 Gio, les plus anciens
partent) par le client HTTP épinglé — Media Foundation ne saurait pas
parler à notre certificat auto-signé, et un clip fait 30 Mo : quelques
secondes de barre de progression, puis l'avance est instantanée.

### La normalisation côté serveur

Toute vidéo partagée (`.mp4 .mov .webm .mkv .m4v .avi`) est **refaite par
ffmpeg** en MP4 H.264 High (sans B) + AAC, 1080p maximum, `+faststart`,
rotation des vidéos de téléphone appliquée, plus un **poster** JPEG et un
`meta.json` (durée, dimensions). Un clip enregistré par ki-chat qui est
déjà H.264/AAC sous 10 Mbit/s est simplement **recopié** (`-c copy`), sans
réencodage. Une file de travaux, **un ffmpeg à la fois**, `nice`, délai
maximal, `kill` ; pendant ce temps le message dit « vidéo en
préparation… ». Les HEVC d'iPhone, les WebM, tout devient lisible par le
même décodeur chez tout le monde.

### Le partage

Le téléversement actuel (un bloc de 25 Mo maximum) ne suffit pas à un clip
ni à une vidéo de téléphone. **Téléversement par morceaux** de 8 Mo
(`POST /files/partiel?upload=…&index=…`, puis `…/fin`), sur un fil de
fond, avec barre de progression et reprise ; chaque requête reste sous la
limite du routeur. Une alternative serait un flux QUIC sur la connexion
existante ; on garde HTTP pour ne pas mêler un gros envoi au circuit de la
voix.

Partager un clip : depuis la galerie ou la pastille juste après la prise,
on choisit le salon (par défaut le salon texte courant, ou le fil de jeu),
une légende ; l'original monte une fois (30 s ≈ 45 Mo à 12 Mbit/s) ; le
serveur le range dans `data/clips/<id>/source.mp4`, fabrique
`partage.mp4` (≤ 8 Mbit/s) et `poster.jpg`, puis poste **au nom du
membre** un message avec le lien : chez chacun, une carte vidéo (poster,
durée, ▶) qui s'ouvre dans la visionneuse. L'original reste pour l'atelier.

### L'atelier et l'export téléphone

L'atelier tourne **dans le client** pour tout ce qui se voit (aperçu,
réglages) et **sur le serveur** pour tout ce qui se fabrique (ffmpeg) :

- **Aperçu** : le décodeur de la visionneuse, dessiné dans un cadre 16:9
  ou 9:16 ; le recadrage est un rectangle UV sur la texture (gratuit) ; le
  « fond flou » de l'aperçu est une copie réduite à 96 px agrandie avec
  filtrage (un faux flou qui suffit à juger) ; une bande de vignettes sous
  la barre de temps (décodées une fois à l'ouverture), deux poignées
  début/fin, lecture de la sélection.
- **Formats** : *Original* (16:9, coupé) et *Téléphone* (1080×1920). Trois
  mises en page téléphone : **Recadré** (une fenêtre 9:16 qu'on déplace,
  avec une position de fin optionnelle pour suivre l'action — déplacement
  linéaire), **Fond flou** (la vidéo entière au milieu, elle-même floutée
  et agrandie derrière), **Zoom** (recadré avec un facteur 1,0-2,0).
- **Son** : un curseur par piste (jeu, micro, vocal), coupure ; le résultat
  est mixé en une stéréo.
- **Titre** : un texte, position haut/bas, police livrée avec l'image
  Docker (`fonts-dejavu-core`, ~3 Mo) — le `drawtext` de ffmpeg
  (**à vérifier au C3** que le build BtbN l'a, et ajouter `ffprobe` de
  la même archive ou lire les durées dans la sortie de ffmpeg).
- **Export** : la recette part au serveur (`ClipExporter`), qui construit
  la ligne ffmpeg (`trim`, `crop`, `scale`, `boxblur`+`overlay`,
  `drawtext`, `amix` avec volumes), x264 `-preset veryfast` (réglable),
  AAC 160 kbit/s, `+faststart`, cadence de la source ; progression
  renvoyée (`-progress pipe:1`) ; résultat dans `telephone.mp4`. Puis
  trois boutons : **Enregistrer sous…**, **Partager dans un salon**,
  **Envoyer sur le téléphone**.

TikTok et Instagram acceptent le MP4 H.264 + AAC en 1080×1920 ; leurs
durées maximales (Reels ~3 min, TikTok 10 min) sont loin au-dessus de nos
clips — **à vérifier au C3**, ces limites bougent.

Si le serveur s'avère trop faible pour x264 (voir Questions), l'export
peut se faire **dans le client** par le même module Media Foundation
(Sink Writer avec encodeur H.264 + AAC) et les filtres refaits chez nous
(recadrage/échelle/flou sur CPU ou en D3D11) — c'est plus de travail, on
ne le fait que si les mesures l'imposent.

### Le téléphone

Le client affiche un **QR code** (`qrcode`, rendu en texture) d'un lien
`https://<serveur>/clips/tel/<jeton>` valable une heure ; le téléphone
scanne, télécharge le MP4, et le partage sur TikTok/Instagram avec sa
feuille de partage. Le certificat auto-signé fera **avertir le
navigateur du téléphone une fois** (« connexion non privée » → continuer) ;
un vrai certificat (Let's Encrypt) sur le nom du serveur effacerait ça —
question posée.

## Interface

- **Barre du haut** : un point « ● REC » quand l'enregistreur tourne
  (info-bulle : tampon, résolution, encodeur) ; clic = marche/arrêt.
- **Paramètres → onglet « Clips »** : enregistreur au démarrage, touche,
  durée du tampon (15/30/60/120 s), qualité (Équilibré 12 / Qualité 20 /
  Léger 720p60 8 Mbit/s), cadence (30/60), source (automatique/écran/
  fenêtre), pistes (jeu, micro, vocal), dossier, son de confirmation.
- **Page « Clips »** (bouton à côté de « Valorant ») : grille de vignettes,
  durée, date, taille, « avec les voix » ; tri par date ; clic = lire ;
  clic droit = Lire, Partager…, Modifier…, Renommer, Ouvrir le dossier,
  Supprimer ; une jauge de place du dossier.
- **Dans le chat** : carte vidéo (poster + durée + ▶) sous le message,
  comme l'aperçu d'image ; « en préparation… » tant que le serveur
  travaille.
- **La visionneuse** : un voile sombre plein cadre ; image : ajustée, molette
  = zoom, glisser = déplacer, ←/→ = média précédent/suivant du salon,
  Échap ; vidéo : ▶/⏸ (espace), barre d'avance (clic/glisser), temps,
  volume propre (mémorisé) + coupure, boucle, ←/→ = ±5 s ; boutons
  Enregistrer sous…, Copier (images), Ouvrir dans le navigateur (l'ancien
  chemin, gardé).
- **L'atelier** : aperçu à gauche (16:9 ⇄ 9:16), réglages à droite (format,
  mise en page, position de fin, titre, curseurs audio), bande de temps en
  bas avec vignettes et poignées, Exporter avec progression, puis les trois
  boutons.

## Stockage

- **Client** : `Vidéos\ki-chat\` (clips, jamais supprimés par ki-chat),
  `%APPDATA%\ki-chat\clips\` (vignettes, `.json`), `%LOCALAPPDATA%\ki-chat\
  cache\medias\` (fichiers du serveur, 1 Gio, LRU).
- **Serveur** : `data/clips/<id>/{source.mp4, partage.mp4, poster.jpg,
  telephone.mp4, meta.json}`, quota **à part** des fichiers
  (`KI_CLIPS_MAX_BYTES`, 8 Gio proposés ; `KI_CLIPS_TTL_DAYS`, 60 jours),
  purge horaire comme `files.rs`. Les vidéos partagées « à la main » gardent
  le circuit des fichiers (25 Mo → par morceaux, normalisées).

## Protocole (esquisse)

- HTTP : `POST /files/partiel?upload=…&index=…` et `…/fin?name=…` (jeton
  voix en en-tête, comme aujourd'hui) ; `GET /files/{id}/{name}` inchangé
  (+ `Range` un jour) ; `GET /clips/{id}/poster.jpg` ; `GET /clips/tel/
  {jeton}`.
- `ClientMsg::ClipPartager { upload, channel, legende, pistes }`,
  `ClipExporter { clip, recette }`, `ClipSupprimer { clip }`.
- `Recette { debut_ms, fin_ms, format: Original | Telephone { cadre:
  Recadre { x, y, w, h, fin: Option<(x, y)> } | FondFlou | Zoom { x, y,
  facteur } }, titre: Option<{ texte, position }>, audio: { jeu, micro,
  vocal }, cadence }` — chaque champ borné et validé par le serveur.
- `ServerMsg::ClipEtat { clip, etat: EnPreparation { pour_cent } | Pret
  { url, poster, duree_s } | Erreur(String) }`, `ClipTelephone { url,
  expire_s }`.
- Un message de chat avec une vidéo reste un message avec un lien : la
  carte vient de l'extension et du `meta.json`, comme l'aperçu d'image.

## Budget

- **Mémoire** (tampon) : débit × durée ÷ 8 — 12 Mbit/s × 30 s = 45 Mo,
  20 Mbit/s × 120 s = 300 Mo ; audio 384 ko/s par piste, 35 Mo pour trois
  pistes de 30 s.
- **Processeur en jeu** : le chemin actuel copie la trame en RAM,
  convertit en I420 (SIMD) et la remonte en NV12 : ~3-4 ms par image, soit
  ~20 % d'un cœur à 60 images/s. Acceptable pour commencer ; le **chemin
  tout-GPU** (texture WGC → NVENC sur le même device, entrée BGRA) ramène
  ça à zéro — C4, si le profileur le demande.
- **Carte graphique** : NVENC à 1080p60 est fait pour ça (c'est ShadowPlay) ;
  deux sessions (diffusion + clips) tiennent dans la limite des GeForce.
- **Disque** : 45 Mo par clip de 30 s en équilibré ; la jauge le dit.
- **Réseau** : un partage = l'original une fois (45 Mo → ~20 s à 20 Mbit/s
  montants) ; l'export ne renvoie rien.
- **Serveur** : x264 `veryfast` sur un clip 1080p60 de 30 s, ~15-60 s selon
  le processeur ; une tâche à la fois, `nice` ; mesure au C2 sur le vrai
  serveur.

## Jalons

### C0 — La visionneuse
`ki-media` (Media Foundation Source Reader, Windows), la file « médias » du
moteur vocal, la visionneuse (images : zoom, déplacement, suivant/
précédent, enregistrer sous, copier ; vidéos : lecture, avance, volume),
la carte vidéo dans le chat, le cache disque, le téléversement par
morceaux, la normalisation ffmpeg + poster côté serveur, `.webp` et GIF
animés. **Validation** : une vidéo d'iPhone (HEVC, tournée en portrait)
glissée dans le chat se lit chez tout le monde, droite, avec le son, sans
navigateur ; une image de 8000 px s'ouvre sans figer ; on avance à la
seconde près ; les copains n'entendent pas la vidéo par le micro.
Estimation : 3 sessions.

### C1 — L'enregistreur
Profil NVENC « clip », le tampon, les robinets audio (jeu, micro, vocal),
la touche, le Sink Writer (H.264 tel quel + AAC), le son de confirmation,
l'overlay, la page « Clips » (galerie locale, vignettes, lire dans la
visionneuse, supprimer, ouvrir le dossier), l'onglet Paramètres, le
marqueur de plantage. **Validation** : une soirée VALORANT avec
l'enregistreur en marche — pas de saccade sentie, ~20 % d'un cœur au plus,
un clip par appui, 29-30 s, son et image synchrones, lisible dans
l'Explorateur et sur un téléphone ; ki-chat fermé brutalement ne laisse
pas de fichier cassé. Estimation : 3 sessions.

### C2 — Le partage
`ClipPartager`, `data/clips/`, quota et purge, `partage.mp4` + poster,
message au nom du membre, carte dans le chat, progression, quotas dans
`/diag-resume`. **Validation** : un clip partagé depuis la galerie apparaît
chez tous en moins d'une minute et se lit dans la visionneuse ; le stock
plein est refusé proprement ; la permission « Partager des fichiers »
s'applique. Estimation : 1-2 sessions.

### C3 — L'atelier et le téléphone
L'atelier (coupe, formats, trois mises en page, position de fin, titre,
curseurs audio), `ClipExporter` + recette validée + ffmpeg, progression,
`telephone.mp4`, QR code et lien à jeton. **Validation** : un clip coupé
et recadré en 9:16 arrive sur un téléphone par le QR code et se publie sur
TikTok et Instagram sans que la plateforme le refuse ni le réencode
bizarrement (image nette, son présent) ; la police du titre s'affiche ;
une recette hors bornes est refusée par le serveur. Estimation : 3-4
sessions.

### C4 — Le confort
Encodeur AMD/Intel par la transformée H.264 de Media Foundation (pour les
copains sans NVIDIA — et la diffusion en profiterait), chemin tout-GPU
(texture WGC → NVENC), une seule capture pour diffusion + clips, chemin
portable (macOS) de la visionneuse, décodage DXVA, `Range` HTTP, export
côté client si le serveur ne suit pas, bouton de manette pour la touche,
un récap « clips de la semaine » dans le fil de jeu. Au fil de l'eau.

## Risques et parades

- **NVENC fragile chez certains** (le GTX 1080 de Cheekyyyy a eu un écran
  bleu en diffusion ; INVALID_DEVICE chez jildhorn) → l'enregistreur suit
  la même politique que la diffusion (deux refus → arrêt et message,
  marqueur de plantage, jamais de logiciel à 1080p60), et reste éteint par
  défaut.
- **Pas de NVIDIA** → 720p30 logiciel ou rien en C1 ; l'encodeur Media
  Foundation en C4.
- **Le Sink Writer refuse le H.264 tel quel** → plan B `mp4` + AAC par la
  transformée MF (une journée).
- **Vanguard** → rien d'injecté (WGC, sondage clavier, overlay séparé) :
  c'est déjà le cas de la diffusion et de l'overlay.
- **Windows 10 d'avant 2004** → pas de boucle par processus : une seule
  piste « système » (avec les voix), et l'interface le dit.
- **Serveur trop lent pour x264** → `ultrafast`, 30 images/s, ou export
  côté client (C4).
- **Certificat auto-signé sur le téléphone** → avertissement une fois ;
  Let's Encrypt si drion veut.
- **Disque du serveur** → quota à part, purge, `/diag-resume`.
- **Vie privée** → fenêtre du jeu par défaut, voix des copains sur option,
  rien n'est envoyé sans geste, les diagnostics n'ont rien.
- **Un clip de 10 min par erreur** → la durée du tampon est bornée à
  120 s ; la galerie montre la taille.

## Questions ouvertes (posées à drion le 2026-09-14)

1. **Cartes graphiques** : tout le monde est en NVIDIA ? Qui a AMD ou
   Intel ? (Défaut : NVENC seul en C1, les autres en C4.)
2. **Windows** : tout le monde en Windows 10 2004+ ou 11 ? Combien de
   Mac ? (Défaut : clips Windows seulement ; visionneuse Mac en C4.)
3. **Source et réglages** : fenêtre du jeu automatiquement quand VALORANT
   tourne, sinon l'écran principal ; 1080p60 ; tampon 30 s par défaut,
   réglable 15/30/60/120 s. Ça va ?
4. **Pistes audio** : jeu + micro par défaut, voix des copains en option
   (cochée ou décochée par défaut ?), pistes séparées dans le fichier.
   Qu'est-ce que vous voulez entendre dans un clip partagé ?
5. **Touche** : Alt+F10 comme NVIDIA, ou autre ? Certains gardent
   ShadowPlay actif (même touche = deux clips) ? Retour : un son + l'overlay.
6. **Dossier** : `Vidéos\ki-chat\`, rien n'est supprimé automatiquement,
   une jauge de place. Ça va ?
7. **Serveur** : combien de CPU, de RAM et de disque sur le Jelastic ?
   (Décide si l'export téléphone se fait sur le serveur — ma préférence —
   et le quota des clips : 8 Gio et 60 jours proposés, à part des
   fichiers.) Et vos connexions montantes : fibre partout ?
8. **Certificat** : l'avertissement sur le téléphone est acceptable, ou on
   met un vrai certificat (Let's Encrypt) sur le nom du serveur ?
9. **Atelier v1** : coupe + format téléphone (recadré déplaçable / fond
   flou / zoom) + niveaux audio + un titre. Ralenti, musique, sous-titres,
   webcam : plus tard. Il manque quelque chose d'essentiel pour vos
   TikTok ?
10. **Ordre** : je propose la visionneuse d'abord (C0) — l'atelier en a
    besoin et c'est utile tout de suite —, puis la touche (C1). Ou la
    touche d'abord ?
