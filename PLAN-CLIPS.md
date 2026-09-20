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
- **Les voix des copains** sont une piste à part, que l'on peut retirer
  de chaque clip avant de le partager ; l'option est visible dans les
  réglages (cochée par défaut : décision de drion) et la galerie dit
  « avec les voix ».
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
   (pas de répétition tant qu'on tient), **combinaison réglable** dans les
   Paramètres (« appuie sur ta combinaison », comme la touche du
   push-to-talk), Alt+F10 par défaut.
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

- **Client** : `Vidéos\ki-chat\` (clips, jamais supprimés par ki-chat ;
  le dossier se change dans les Paramètres — le pointer sur un dossier
  synchronisé par Google Drive pour ordinateur suffit à envoyer les clips
  dans le Drive, sans que ki-chat sache parler à Google),
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

### C0 — La visionneuse — livré (0.1.35, 2026-09-14)
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

**Fait le 2026-09-14** (une session), **vérifié** par les tests :
- `crates/media` (`ki-media`) : Source Reader en NV12 + float aux cadence
  et voies natives, rééchantillonnage cubique maison vers 48 kHz (le lecteur
  ne rééchantillonne pas), ouverture d'affichage lue (les 1088 lignes d'un
  1080p), DXVA coupé ; tests sur des fichiers fabriqués par le ffmpeg de la
  machine (images, son, recherche, fichier muet).
- `ki_voice::medias` : la file « médias » (mono 48 kHz, pause, horloge =
  échantillons partis vers la carte, consommateur désigné), mixée dans le
  rappel de sortie du moteur avant le volume général et le limiteur, et la
  sortie à part hors salon (`SortieSeule`, natif ou cpal).
- Client : `visionneuse.rs` (voile, image avec zoom/déplacement, vidéo
  avec un fil de lecture par fichier, curseur, ←/→, volume mémorisé,
  boucle, enregistrer sous, copier, navigateur), `medias.rs` (cache disque
  1 Gio LRU, téléchargement suivi, fiche `meta.json`), carte vidéo dans le
  fil (poster, durée, « en préparation »), GIF et WebP animés (bornés à
  48 Mpx et 400 images), téléversement par morceaux de 8 Mo (512 Mo max).
- Serveur : `medias.rs` (`/upload/partiel`, `/upload/fin`, normalisation
  ffmpeg une à la fois — testée : un HEVC portrait 44,1 kHz mono devient
  H.264/AAC 48 kHz stéréo avec poster ; un H.264 propre est recopié —,
  reprise après redémarrage, purge des morceaux abandonnés), types MIME et
  `inline` au téléchargement, ffprobe dans l'image Docker,
  `KI_FILES_MAX_FILE_MB`.
- Non fait, volontairement : le décodage DXVA (logiciel suffit), la
  rotation d'un fichier local (le serveur la cuit), `Range` HTTP.

Reste à valider par drion (la liste de validation ci-dessus) — et à voir
à l'usage : le son de la vidéo hors salon sort-il bien par le bon casque
(la sortie à part suit le périphérique réglé), et l'image tient-elle
60 images/s sur les portables.

### C1 — L'enregistreur — livré (0.1.35, 2026-09-14)
Profil NVENC « clip », le tampon, les robinets audio (jeu, micro, vocal),
la touche, le Sink Writer (H.264 tel quel + AAC), le son de confirmation,
l'overlay, la page « Clips » (galerie locale, vignettes, lire dans la
visionneuse, supprimer, ouvrir le dossier), l'onglet Paramètres, le
marqueur de plantage. **Validation** : une soirée VALORANT avec
l'enregistreur en marche — pas de saccade sentie, ~20 % d'un cœur au plus,
un clip par appui, 29-30 s, son et image synchrones, lisible dans
l'Explorateur et sur un téléphone ; ki-chat fermé brutalement ne laisse
pas de fichier cassé. Estimation : 3 sessions.

**Fait le 2026-09-14** (une session), **vérifié** par les tests :
- `ki-media` écrit des MP4 par le Sink Writer : H.264 tel quel (le type
  d'entrée est le type de sortie, aucun encodeur inséré — **vérifié**, le
  fichier se relit), PCM 16 bits → AAC 160 kbit/s par piste, plusieurs
  pistes ; `annexb` découpe les NAL, retrouve SPS/PPS et les trames clés.
  Surprise **vérifiée** : la source MPEG-4 de Media Foundation énumère les
  pistes à l'envers de l'ordre du fichier — le lecteur prend maintenant le
  dernier flux de chaque type, sinon un clip s'ouvrait sur sa piste muette.
- `ki-video` : la longueur du GOP se règle (`gop_s`, 2 pour diffuser, 1
  pour les clips) ; `ki-voice` : les robinets « micro » et « copains » sur
  le moteur (`Robinet`, trames mono de 20 ms, verrou bref sur le fil temps
  réel) et `SonSysteme`, la boucle « tout sauf ki-chat » brute.
- Client `clips.rs` : réglages persistés, tampon de trames encodées coupé
  à la trame clé (une seconde de marge), trois anneaux de son horodatés
  sur l'origine de la vidéo (trous comblés de silence), photographie à
  l'appui, fil d'écriture (mélange écrêté en première piste, puis chaque
  source ; images et son entrelacés par le temps ; SPS/PPS recollés si la
  première image ne les porte pas), vignette JPEG dans le cache, espace
  disque vérifié (500 Mo), source automatique par l'exécutable du jeu
  (dix titres connus), bascule sur l'écran quand la fenêtre disparaît, et
  reprise de la fenêtre du jeu quand il arrive.
- Le raccourci : `ptt::Raccourci` (Ctrl/Alt/Maj + F1-F12, lettres,
  chiffres, Inser…), « appuie sur ta combinaison » dans les réglages ;
  Alt+F10 par défaut. **Retour de drion (2026-09-14) : en jeu sous VALORANT,
  le raccourci ne partait pas** — le sondage du clavier (`GetAsyncKeyState`,
  celui du push-to-talk) ne voit plus rien quand la fenêtre au premier plan
  est plus privilégiée que nous (un jeu sous anti-triche), la même barrière
  qui oblige Discord ou OBS à « tourner en administrateur ». Corrigé par
  `raccourci.rs` : la combinaison est tenue auprès de Windows
  (`RegisterHotKey`, le mécanisme d'Alt+Tab, résolu par le système avant que
  la touche n'atteigne le jeu — plein écran exclusif compris, sans
  élévation), sur un fil à part qui **déclenche le clip lui-même** sans
  passer par l'interface (réduite derrière le jeu, elle peut ne pas
  repeindre) ; le son de confirmation part du fil d'écriture. Ctrl ou Maj
  tenus en plus (accroupi, en marche) ne gênent pas une touche de fonction
  — les variantes sont enregistrées aussi ; jamais sur une touche qui écrit
  (Ctrl+Alt+E, c'est AltGr+E). Si Windows refuse la combinaison (déjà prise
  par un autre programme), le sondage reprend et les réglages le disent en
  orange. **Validé en jeu par drion le 2026-09-14** (Ctrl+Maj+1 : clavier
  55 %, sans rangée de touches F).
- Interface : onglet Réglages → Clips, bouton « Clips » et point « REC »
  à côté de « Valorant », page Clips (galerie en vignettes, lecture dans la
  visionneuse avec ←/→ entre les clips, voir dans le dossier, suppression
  confirmée, « Clip ! »), son « clip » de confirmation, ligne « Clip
  enregistré » dans l'overlay, marqueur `clips.en-cours` (mort brutale →
  l'enregistreur reste éteint au démarrage suivant et le dit).
- Test de bout en bout sur la machine de développement (`cargo test -p
  ki-client-gui enregistre -- --ignored`) : l'écran filmé quatre secondes
  en NVENC, le clip écrit en 720p avec sa piste son, relu image par image.
- Non fait, volontairement : le profil NVENC « qualité » (c'est celui de la
  diffusion, CBR, à revoir si l'image déçoit), une seule capture pour
  diffusion + clips, le retour visuel en plein écran exclusif (rien ne
  peut s'y afficher, l'overlay non plus).

Reste à valider par drion (la liste ci-dessus) — et surtout une vraie
soirée : la charge en jeu, la synchronisation image/son sur un clip de
trente secondes, le micro et les copains dans le fichier.

### C2 — Le partage — livré (0.1.37, 2026-09-15), à valider en soirée
`ClipPartager`, `data/clips/`, quota et purge, `partage.mp4` + poster,
message au nom du membre, carte dans le chat, progression, quotas dans
`/diag-resume`. **Validation** : un clip partagé depuis la galerie apparaît
chez tous en moins d'une minute et se lit dans la visionneuse ; le stock
plein est refusé proprement ; la permission « Partager des fichiers »
s'applique. Estimation : 1-2 sessions.

**Fait le 2026-09-14** (une session), **vérifié** par les tests :
- Serveur `clips.rs` : `POST /clips/fin?upload=…&parts=…` avec un corps
  JSON (salon, légende ≤ 500 caractères, pistes, voix, nom) — les morceaux
  passent par `/upload/partiel` comme toute vidéo ; l'assemblage (partagé
  avec `medias.rs`, `assembler`) range la source dans
  `data/clips/<id>/source.mp4`, sous le plafond **à part**
  (`KI_CLIPS_MAX_BYTES`, 8 Gio ; `KI_CLIPS_TTL_DAYS`, 60 jours ; purge
  horaire par `files::sweep`, reprise au démarrage) ; la fiche `meta.json`
  porte `clip`, `garder_source`, `auteur`, `salon`, `legende`, `nom`,
  `pistes`, `voix`. La même fabrique convertit ; **la source reste** (pour
  l'atelier, C3). Le serveur poste **au nom du membre** (`poster_membre`)
  la légende et le lien, dans le salon demandé — textuel, visible de lui,
  avec « Écrire » ; sans ffmpeg, refus (503) : la source porte les pistes
  séparées, elle ne se livre pas telle quelle.
- Écart avec l'esquisse : pas de `ClientMsg::ClipPartager` — tout passe par
  HTTP, et les clips se servent par **`/files/<id>/…`** (le téléchargement
  regarde `data/files/` puis `data/clips/`) : les clients n'ont rien à
  apprendre, la carte vidéo et la visionneuse de C0 marchent telles quelles,
  y compris chez qui n'a pas encore mis à jour. Le fichier partagé porte le
  nom du clip (`2026-09-14_21h03m12_VALORANT.mp4`), pas `partage.mp4`.
- Le son de la version partagée (**vérifié** par un test ffmpeg avec quatre
  pistes) : **une seule piste** — le mélange (`0:a:0`, copié), ou, « sans
  les voix des copains », un mélange refait des autres (`amix` du jeu et du
  micro, ou l'une seule), ou muet ; les pistes séparées ne quittent jamais
  le dossier du clip. Une vidéo ordinaire garde tout, comme avant. La copie
  sans réencodage se décide sur le débit de la **piste vidéo** (13 Mbit/s
  au plus) : un clip « équilibré » passe tel quel, en quelques secondes.
- Client : `clips::Fiche` (`%APPDATA%\ki-chat\clips\<fnv>.json`, écrite
  avec la vignette : pistes, durée, source) ; galerie → clic droit →
  « Partager dans un salon… » : la boîte (salon textuel, courant par
  défaut ; légende ; « avec les voix des copains » si le clip en a ;
  progression ; erreurs) ; l'envoi par morceaux est partagé avec le
  trombone (`envoyer_morceaux`). Permission « Partager des fichiers »
  vérifiée des deux côtés. Un serveur d'avant C2 : « mise à jour
  nécessaire ». Suppression d'un clip = fichier + vignette + fiche.
- `/diag-resume` : un paragraphe « stockage » — fichiers et clips, poids,
  plafond, âge.

Reste à valider par drion : un clip partagé depuis la galerie apparaît chez
tous, se lit dans la visionneuse, avec et sans les voix ; le message au
nom du membre ; le serveur de prod mis à jour (Watchtower à la prochaine
release) — avant, « mise à jour nécessaire ».

### C3 — L'atelier et le téléphone — livré (0.1.38, 2026-09-15)
L'atelier (coupe, formats, trois mises en page, position de fin, titre,
curseurs audio), `ClipExporter` + recette validée + ffmpeg, progression,
`telephone.mp4`, QR code et lien à jeton. **Validation** : un clip coupé
et recadré en 9:16 arrive sur un téléphone par le QR code et se publie sur
TikTok et Instagram sans que la plateforme le refuse ni le réencode
bizarrement (image nette, son présent) ; la police du titre s'affiche ;
une recette hors bornes est refusée par le serveur. Estimation : 3-4
sessions.

**Fait le 2026-09-15** (une session), **vérifié** par les tests :
- Serveur `export.rs` : la `Recette` (JSON : `debut_ms`, `fin_ms`,
  `format` = `original` | `telephone` + `cadre` = `recadre {x, fin_x?}` |
  `fond_flou` | `zoom {x, y, facteur}`, `titre {texte, position}`,
  `audio {jeu, micro, copains}` 0–2, `cadence` 0/24/25/30/50/60) validée
  contre la sonde de la source (bornes 0,5 s – 3 min, cadre dans l'image,
  zoom 1–2, titre ≤ 80 sans caractère de contrôle, police présente) ;
  la ligne ffmpeg composée ici : `-ss`/`-t`, `crop` (glissement par le
  numéro d'image `n`), `split`+`boxblur`+`overlay`, `crop`+`scale` 1080×1920,
  `fps`, `drawtext` avec `textfile` sans expansion (le texte ne passe pas
  par la ligne de commande), `volume` par piste + `amix normalize=0`, x264
  veryfast crf 21, AAC 160 k, `+faststart`, `-progress pipe:1` suivi sur
  le tube → `export.json` (`en_attente`/`en_cours`/`pret`/`erreur`,
  pour cent, fichier, dimensions). **Test ffmpeg de bout en bout** :
  source 720p à quatre pistes → `telephone.mp4` 1080×1920, une piste,
  durée, titre avec la police locale ; une recette hors bornes est refusée
  et l'état le dit.
- Routes (celui qui a déposé, ou un admin) : `POST /clips/{id}/exporter`
  (202, un export à la fois par clip), `POST /clips/{id}/telephone`
  (`{fichier}` → `{url, expire_s}` : jeton 128 bits, une heure, un seul
  fichier, en mémoire), `GET /tel/{jeton}` (pièce jointe sous un nom
  parlant `…-tiktok.mp4`), `POST /clips/{id}/partager` (`{channel,
  legende, fichier}`), `DELETE /clips/{id}`. `POST /clips/fin` sans
  `channel` = dépôt pour l'atelier, sans message. L'image Docker embarque
  `fonts-dejavu-core` ; `KI_POLICE` pour une autre police. Un export
  interrompu par un redémarrage est marqué en erreur au démarrage.
- Client `atelier.rs` : plein cadre comme la visionneuse ; l'aperçu (le
  `Lecture` de la visionneuse, désormais `pub(crate)`) dans un cadre 16:9
  ou 9:16, recadré = rectangle UV glissable (avec « suivre l'action » :
  la position de fin, et l'aperçu glisse en lisant), fond flou = copie
  24 px agrandie avec filtrage, zoom = UV plus petit ; le titre peint
  comme ffmpeg le posera ; bande de temps (12 vignettes par `ki_media`
  sur un fil, poignées début/fin, tête, sélection en boucle, « début
  ici »/« fin ici ») ; panneau (format, mise en page, titre, un curseur par
  piste de la fiche, cadence) ; Exporter (dépôt sans salon si la fiche ne
  connaît pas ce serveur, puis recette, puis `export.json` chaque seconde),
  puis « Enregistrer sous… » (`medias::telecharger`), « Partager dans un
  salon », « Envoyer sur le téléphone » (QR code par `qrcode` 0.14, sans
  ses features, dessiné module par module, « copier le lien »). Fiche :
  `pistes` devenu `Option`, `serveur` + `serveur_base` notés au partage
  (C2) comme au dépôt. Galerie → « Modifier dans l'atelier… » ; Échap
  ferme l'atelier d'abord.
- Fait ensuite (C4 et finitions du 2026-09-15) : « Retirer du serveur »
  dans la galerie ; **le message du fil part avec le clip** — la fiche
  retient les messages postés pour lui (`messages: [(salon, ts)]`, au
  partage comme au repartage), et `DELETE /clips/{id}` les efface de
  l'historique et le dit à tout le monde (`MessageDeleted`). Non fait : le
  lien téléphone pour la version partagée (la route l'accepte : `fichier`
  = la sortie du partage).
- Retours de drion (2026-09-15, après essai) : **« Resserré »**, quatrième
  mise en page — une fenêtre plus large que le 9:16, serrée dans le cadre
  (`crop` puis `scale=1080:1920` sans garder le rapport) ; on garde presque
  tout, un peu déformé, le curseur dit la part de largeur gardée. Et le
  lien du QR code disait `127.0.0.1` quand le serveur tourne sur ce PC : le
  client y met l'adresse du PC sur le réseau local (même Wi-Fi), et
  `KI_PUBLIC_URL` côté serveur fixe l'adresse publique s'il le faut.

Reste à valider par drion : l'aperçu et les quatre mises en page, la coupe,
un export 9:16 avec titre, le QR code lu par le téléphone (l'avertissement
du certificat, une fois), la publication sur TikTok et Instagram — image
nette, son présent, rien de refusé —, et la police dans l'image Docker au
premier export en prod.

### C4 — Le confort — en cours (0.1.38 : profil NVENC des clips, retrait du serveur, yt-dlp à jour)
Encodeur AMD/Intel par la transformée H.264 de Media Foundation (pour les
copains sans NVIDIA — et la diffusion en profiterait), chemin tout-GPU
(texture WGC → NVENC), une seule capture pour diffusion + clips, chemin
portable (macOS) de la visionneuse, décodage DXVA, `Range` HTTP (fait en
0.1.43, voir plus bas), export côté client si le serveur ne suit pas,
bouton de manette pour la touche,
un récap « clips de la semaine » dans le fil de jeu. Au fil de l'eau.

**Fait le 2026-09-15**, **vérifié** par les tests :
- **Le profil NVENC des clips** (`ki_video::Profil::{Diffusion, Clip}`,
  dans `StreamConfig` et `creer_encodeur`) : P4 accordé « haute qualité »,
  VBR (crête 1,5×), VBV d'une seconde, deux passes en pleine résolution,
  AQ spatiale et temporelle, profil High ; la diffusion ne change pas
  (P4 faible latence, CBR, Main). Refus de la carte → profil diffusion,
  puis logiciel. Pas de lookahead : l'enveloppe NVENC est synchrone (une
  trame entre, une trame sort). Le préréglage reste P4 : les GUID P5-P7 ne
  sont pas dans nos liaisons et ne se devinent pas. **Vérifié** sur la
  machine de développement (test qui filme l'écran) : NVENC accepte le
  profil, le fichier est plus léger.
- **« Retirer du serveur »** dans la galerie (clip dont la fiche connaît
  un serveur) : confirmation, `DELETE /clips/{id}`, la fiche oublie le
  serveur (un 404 vaut un retrait : purgé entre-temps). Le message du fil
  garde un lien mort — dit dans la confirmation.
- **yt-dlp se tient à jour** (PLAN-MUSIQUE M4, `serveur/ytdlp.rs`) : voir
  ce plan-là.

Écartés, par choix : l'encodeur AMD/Intel (tout le monde est en NVIDIA),
le chemin tout-GPU et la capture unique (« si le profileur le demande »,
et personne ne sent la charge), macOS (laissé de côté par drion), la
manette, le récap hebdo (à voir si l'envie vient).

### Robustesse du partage (0.1.43)

Le rapport « quand on modifie un clip, on ne peut pas le partager dans un
salon : ça charge à l'infini » venait du serveur du groupe passé sur un
conteneur Jelastic à CPU limité, où un export qui prenait cinq secondes
sur le PC de dev en prend des minutes — et de plusieurs façons, pour
l'atelier, de rester sans réponse pendant ce temps. Corrigé des deux
côtés, tout compatible avec les clients 0.1.42 :

- **La fabrique est premier arrivé, premier servi** (`VecDeque`, plus le
  `Vec::pop` qui servait le dernier déposé d'abord) ; elle sait ce qu'elle
  a en main (`export_vivant`), le tableau de bord et `/diag-resume` le
  montrent (« fabrique : N en file · export de <id> depuis 3 min »,
  `TableauAdmin.fabrique`, `#[serde(default)]`).
- **L'export ne réencode plus une simple coupe** (16:9, sans titre ni
  cadence, source H.264 ≤ 1080p) : `-ss` avant l'entrée et `-c:v copy`,
  coupe à la trame clé qui précède (deux secondes de marge au plus, le GOP
  de l'enregistreur). Le reste passe par x264 `superfast`, sur des fils
  bornés à ce que le conteneur a (`available_parallelism`, huit au plus —
  sans quoi x264 lance cent fils sur un nœud à 64 cœurs et le conteneur
  meurt pour sa mémoire), sous `nice -n 19` pour que la voix passe avant.
  Le délai suit la durée : 120 s + 10 × la durée de la sortie (plafonné à
  une heure), pour la normalisation comme pour l'export, au lieu de 900 s
  fixes.
- **`export.json` est fiable** : `ecrire_etat` et `ecrire_meta` rendent
  l'erreur, `/clips/fin` et `/clips/{id}/exporter` répondent 500
  « stockage indisponible » au lieu de 202 sur un disque plein (le client
  relisait un 404 pendant un quart d'heure) ; une tâche qui meurt écrit
  `erreur` ; un « en cours » que la fabrique ne connaît pas ne bloque plus
  la relance (409) ; chaque état porte `derriere` (tâches devant), `mode`
  (« copie »/« x264 ») et `depuis` (horodatage), facultatifs ; un export
  raté ne laisse pas de fichier à moitié écrit.
- **Les fichiers se servent en flux**, avec `Content-Length`,
  `Accept-Ranges` et `Range` (206, 416) — `/files/<id>/<nom>` comme
  `/tel/<jeton>` — au lieu de lire le fichier entier en mémoire par
  spectateur. Et un dossier de clip ne sert que sa **liste blanche** :
  `meta.json`, `export.json`, `poster.jpg`, la version partagée quand
  elle est prête, les exports qui ne sont pas en train de s'écrire —
  jamais `source.mp4` (les pistes séparées, « sans les voix des
  copains » tenait à un lien) ni `titre.txt`.
- **Le serveur dit ses refus** : `warn!` sur les 401/403 des envois, les
  413 des morceaux, les 507 d'assemblage ; durée de chaque conversion et
  export dans le journal (« prête en 26,6 s, 2 en file ») ; longueur de
  file au dépôt.
- **Côté client, l'atelier ne reste jamais sans réponse** : le jeton est
  relu à chaque requête (`Reseau.jeton`, partagé avec l'application et mis
  à jour au `Welcome` — une reconnexion pendant l'export ne donne plus
  « jeton invalide » au partage) ; l'identifiant du clip est noté dès la
  réponse de `/clips/fin` (un dépôt dont l'attente échoue n'est pas
  renvoyé au prochain essai) ; la barre dit la phase (« dépôt du clip ·
  morceau 3/12 », « en file sur le serveur, 2 devant », « export
  (coupe sans réencodage)… 42 % »), le chrono, et se repeint sans la
  souris ; chaque attente a une borne qui suit la durée (600 s + 15 × la
  durée, une heure au plus), un état figé plus de 15 min ou une fiche
  introuvable plus de 20 s deviennent des erreurs en clair ; `pret`
  n'est accepté que pour le fichier demandé (le `pret` d'un export
  précédent donnait « ce fichier n'existe pas (encore) ») ; un 404
  « clip inconnu » à l'export fait oublier le serveur à la fiche et
  redépose, seul un 404 sans corps vaut « mise à jour nécessaire ».
  Dépôt, export, partage et retrait s'écrivent au journal (lignes
  `clips : atelier …`, `clips : partage …`), avec code et texte serveur
  à chaque échec — lisibles dans `ki-chat.log` et les diagnostics.

Relecture du circuit après coup, sept défauts corrigés (tests à l'appui) :

- **Un export interrompu ne laisse plus de fichier tronqué à partager.**
  `fichier_connu` n'accepte l'export que `export.json` nomme que s'il dit
  `pret` (en cours : il s'écrit ; en erreur : ce qui reste n'a pas d'index)
  ; et partout où l'état est forcé à `erreur` hors `executer` — reprise au
  démarrage (Watchtower recrée le conteneur, ffmpeg meurt avec), tâche
  paniquée, état périmé relancé — `clips::abandonner_export` efface la
  sortie avec, pour que l'autre nom ne la retrouve pas.
- **Sans durée ffprobe** (WebM de MediaRecorder, MKV non finalisé), le
  délai de conversion ne tombe plus à 120 s : `delai_conversion` l'estime
  d'après la taille à 4 Mbit/s, jamais sous les 900 s d'avant.
- **`prochaine()` garde le verrou de la file** en notant la tâche en cours :
  plus de fenêtre où `export_vivant` ne la voit nulle part et où
  `/exporter` repartirait sur un état bien vivant.
- **En file, l'export n'est jamais « figé »** : le serveur n'écrit
  `en_attente` qu'une fois, seul le délai global borne l'attente ; et un
  409 « déjà en cours » se suit (`export.json` dit le fichier) au lieu
  d'échouer puis de tout refaire.
- **La fiche apprend le serveur depuis le fil**, dès la réponse de
  `/clips/fin` (`clips::noter_serveur`), et l'oublie dès un « clip
  inconnu » (`clips::oublier_serveur`) : un atelier fermé pendant le dépôt
  ne redépose plus le clip en double à la réouverture.
- **Clip déjà sur le serveur mais pas prêt** : le chemin rapide relit
  `meta.json` d'abord — `pret` → export, `en_preparation` → on attend en
  phase « préparation sur le serveur », `erreur` ou 404 → la fiche
  l'oublie et on redépose. Fini le « le clip n'est pas prêt » sans issue.
- **Le jeton est relu à chaque morceau** (`envoyer_morceaux` prend une
  closure) — partage, atelier et trombone — et un 401 pendant l'envoi dit
  « la session a été perdue pendant l'envoi — reconnecte-toi, puis
  réessaie » au lieu de « jeton invalide ».

Pas fait, à décider : nommer chaque export avec un horodatage (le cache
client est indexé par l'adresse, un second `telephone.mp4` est servi
depuis l'ancien chez qui a vu le premier) ; un vrai `docker run --cpus=1`
pour rejouer les temps du conteneur.

## Risques et parades

- **NVENC fragile chez certains** (le GTX 1080 de Cheekyyyy a eu un écran
  bleu en diffusion ; INVALID_DEVICE chez jildhorn) → l'enregistreur suit
  la même politique que la diffusion (deux refus → arrêt et message,
  marqueur de plantage, jamais de logiciel à 1080p60), et reste éteint par
  défaut. **Tenu le 2026-09-15** : si l'encodeur logiciel a pris le relais
  au-dessus de 720p30, l'enregistreur s'arrête et le dit (`fatal`) ; et
  une ligne de statistiques part au journal toutes les trente secondes
  (images capturées/encodées, sautées, débit, conversion, encodage,
  tampon, encodeur) pour lire la charge en jeu dans les diagnostics.
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

## Décisions (réponses de drion, 2026-09-14)

- **NVIDIA partout, Windows partout** : NVENC seul, pas de chemin AMD/Intel
  ni de macOS avant qu'un besoin n'arrive.
- **Pistes audio séparées** dans le fichier : une première piste
  *mélange* (pour l'Explorateur, VLC et le téléphone, qui ne lisent que la
  première), puis *jeu*, *micro* et *copains* chacune à part ; l'atelier
  règle les trois.
- **La touche se règle** dans les Paramètres ; Alt+F10 par défaut.
- **Dossier** `Vidéos\ki-chat\`, réglable ; Google Drive par le dossier
  synchronisé, pas par une connexion à Google.
- **Le serveur se dimensionne au besoin** : l'export téléphone se fait sur
  le serveur ; les mesures du C2 diront combien de cœurs.
- **Certificat** : l'avertissement du téléphone est accepté.
- **Atelier v1** validé tel quel.
- **Ordre** : C0 (visionneuse) d'abord, puis C1.
- La question 3 (source, résolution, tampon) n'était pas claire ; ce que
  fait l'enregistreur par défaut : il filme **la fenêtre du jeu** quand il
  la reconnaît (sinon tout l'écran principal), en **1080p à 60 images/s**,
  et garde **30 secondes** ; les trois se changent dans les Paramètres.

## Questions ouvertes (posées à drion le 2026-09-14, réponses ci-dessus)

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
