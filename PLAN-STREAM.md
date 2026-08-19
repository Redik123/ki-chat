# Plan : partage d'écran « Go Live » — ki-chat

> **⏸ CHANTIER EN PAUSE** (décision du 16/08/2026) : la vidéo reprendra quand
> le reste de ki-chat sera jugé parfait. Acquis conservés : S0 (correctifs
> transport, bénéfice vocal permanent), S0.5 (porte de build openh264 : GO)
> et S1a (boucle locale + bouton labo dans ⚙) restent dans le dépôt, prêts
> pour la reprise en S1b.

*Version 2 du 16/08/2026 — nourrie par trois rapports de recherche (capture
Windows, codec, transport QUIC lu dans les sources de quinn), puis passée au
crible par une revue adversariale (15 amendements intégrés). Statut :
**prêt à exécuter après validation**. Estimation honnête : 6-8 sessions.*

## Objectif

Un membre d'un salon vocal clique « Go Live », choisit un jeu (fenêtre) ou un
écran, et les membres du salon peuvent le regarder — image + son du jeu — avec
une latence < 300 ms, pour 2 à 10 spectateurs. Windows uniquement, 1080p30
cible (60 fps plus tard).

## Décisions d'architecture (validées par la revue)

| Sujet | Décision | Pourquoi (résumé) |
|---|---|---|
| Capture vidéo | **windows-capture 2.0.1** (Windows.Graphics.Capture, secours DXGI) | Maintenu (08/2026), fenêtre OU moniteur, buffer CPU prêt à encoder ET texture GPU pour l'avenir ; capture les fenêtres recouvertes |
| Bordure jaune | Désactivée sur Windows 11 (build 20348+), assumée sur Windows 10 | Limitation OS |
| Audio du jeu | **wasapi 0.24** en *process loopback* mode EXCLUDE (tout le système sauf ki-chat) | Technique Discord : le son du jeu SANS renvoyer les voix des copains ; Win10 2004+ |
| Codec | **H.264 via openh264 0.9** (feature `source`, nasm requis), mode `ScreenContentRealTime` | 8-20 ms/frame encode, ~3 ms decode, from-source MSVC comme libopus ; brevets caducs fin 2027, risque nul à notre échelle |
| Encodage matériel | Plus tard (S4), derrière un trait `VideoEncoder` | Aucun wrapper Rust mûr pour Media Foundation ; le logiciel tient le 1080p30 |
| Transport vidéo | **Un flux QUIC unidirectionnel par trame**, `RESET_STREAM` des trames en vol à chaque nouvelle IDR | Fiabilité par trame sans HOL entre trames, délestage par trame au relais ; fragmentation datagramme disqualifiée (19 % d'échec des IDR à 1 % de perte + éviction de la VOIX de la file datagramme) |
| Priorité des flux | Par-stream : `base − (seq − seq_départ)` en **arithmétique saturante**, même `base` pour tous les streams | Le plus ancien d'abord DANS un stream ; round-robin naturel ENTRE streams (deux streamers ne s'écrasent pas) ; le cast `as i32` naïf s'inverserait à 2³¹ |
| Audio du jeu (transport) | Datagrammes `KA` (Opus 48 k stéréo mode musique, 96-128 kbps), jamais muxé | Muxé = hérite du HOL vidéo (200 ms derrière une IDR) ; réutilise jitter buffer et mixeur existants |
| Priorité des médias | Vocal > audio du jeu > vidéo | DATAGRAM écrits avant STREAM dans chaque paquet quinn (composition) + réserve de 4 Kio du tampon datagramme pour la voix. ⚠ Ce n'est PAS une garantie de bout en bout : la vidéo gonfle la file du goulot → validation **chiffrée** (voir S1b) |
| Relais SFU | Une tâche par viewer, file `mpsc(2)`, « jeter les P jusqu'à la prochaine IDR + la demander » | Contre-pression indépendante ; serveur sans état média, ne décode jamais. **Dès S1b** (une diffusion naïve laisserait un viewer lent bloquer tout le monde) |
| Adaptation débit | Goodput mesuré par viewer, min (2ᵉ min si ≥4), × 0,9 du montant streamer ; descente immédiate, montée 1 palier / 5 s | Ladder 8000/4000/2500/1500/1000 kbps ; débit d'abord, résolution ensuite ; fps avant résolution sur contenu écran |
| Sync A/V | `pts_us` de capture commun ; audio du jeu retardé vers la vidéo (D = p95 gigue, 60-200 ms) ; **le vocal n'est jamais retardé** | Comme Discord |
| Chiffrement | XChaCha20-Poly1305 par trame, en-tête clair (32 o vidéo / 24 o audio) = AAD, nonce à octet de domaine (voix 0, vidéo 1, audio jeu 2), `seq` u64 jamais réinitialisé | Une op AEAD par trame ; le serveur route sans pouvoir réécrire ni forger |
| Clé de stream | Générée par le **streamer**. `StreamStarted` (annonce au salon, **sans clé**) distinct de `WatchAccepted { stream_key }` (au seul abonné, après vérif. qu'il est dans le salon vocal du streamer) | La v1 du plan envoyait la clé à tout le salon — corrigé. Niveau 2 (enveloppes X25519 par viewer) possible en S4 |
| Rejoindre en cours | `Watch` → le serveur demande une IDR (≤1/500 ms) → jette les P pour CE viewer jusqu'à l'IDR | ~60-100 ms avant la première image, serveur sans tampon |

## Modèle de threads — explicite (exigence de la revue)

Le point où ce chantier peut casser l'existant : la GUI a UN runtime tokio
`current_thread` qui porte le contrôle ET la pompe de datagrammes voix, et la
boucle de repeint est plafonnée par `request_repaint_after(50 ms)` → 20 fps.

**Streamer** :
- *Thread capture* (windows-capture) : `on_frame_arrived` → pousse le buffer
  BGRA vers le thread encodeur (canal borné à 1, on jette si plein = skip).
- *Thread encodeur* (std) : BGRA→I420 (SIMD) → openh264 → chiffrement →
  `mpsc(2)` vers la tâche d'émission (politique : jeter la plus ancienne).
- *Tâche d'émission* (runtime réseau) : possède la table `seq → SendStream`
  des trames en vol ; `open_uni` + `set_priority` + `write_all` par sous-tâche
  (`FuturesUnordered`) ; au passage d'une IDR : `reset(RESET_STALE)` de masse.
  **L'interface n'est PAS une closure type `DatagramSend`** (un fire-and-forget
  perdrait la contre-pression et rendrait le reset impossible).

**Viewer** :
- *Tâches réseau* (runtime existant) : `accept_uni` + `read_to_end` (I/O pur),
  chiffré transmis par canal std au…
- *Thread décodeur* (std, calque de `voice_feed`) : déchiffre → réordonne par
  `seq` → openh264 decode → YUV→RGB (SIMD) → dépose la `ColorImage` dans un
  `Arc<Mutex<…>>` (pool de 2-3 images réutilisées, pas d'allocation 8 Mo/frame)
  → **`ctx.request_repaint()`** (seul moyen de dépasser le plafond de 20 fps).
- *Thread UI* : prend l'image, `TextureHandle::set`, peint. Compteur de frames
  **réellement peintes** dans les stats.

**Serveur** : tâche d'ingestion par streamer (permis de sémaphore acquis
**avant** de reboucler sur `accept_uni`), tâche de diffusion par viewer.

## Cycle de vie et cas limites (exigence de la revue)

- `end_stream(stream_id)` : appelée depuis `StopStream`, `LeaveVoice` du
  streamer, `disconnect`, et l'échec d'ingestion. `drop_viewer(user_id)` :
  depuis `Unwatch`, `LeaveVoice` du viewer, `disconnect`.
- **Un stream actif max par utilisateur** ; `StartStream` répété = idempotent
  (renvoie le `stream_id` existant). **2 streams max par salon** en v1.
- **Aperçu local du streamer** : depuis le chemin de capture (jamais un
  aller-retour serveur). L'encodeur tourne dès `StartStream` ; zéro viewer =
  pas d'émission réseau (mais l'aperçu vit).
- **Changement de dimensions** (resize fenêtre, jeu qui passe en fenêtré,
  moniteur débranché) : événement de premier plan dès S1a — dimensions
  arrondies aux paires (crop), encodeur recréé, IDR forcée,
  `StreamMetaChanged`, texture et décodeur réinitialisés côté viewer ;
  `on_closed()` → `StopStream` propre + message UI.
- `ConnectedUser.streaming: Option<u32>` pour alimenter le roster.

## Durcissement de l'ingestion (exigence de la revue)

- `max_concurrent_uni_streams(256)` des deux côtés (défaut quinn : 100 —
  plafonnerait le relais).
- Flux entrant d'un compte **sans stream actif** → `stop_sending` immédiat.
- `timeout(1 s)` autour de chaque `read_to_end` ; trame ≤ 4 Mio.
- Limite de débit d'ingestion : 2 × `meta.kbps` (plafond dur 16 Mbps) et
  ≤ 90 trames/s.
- Plafond mémoire global du relais : 32 Mio — **en S1b**, pas après.

## Jalons (redécoupés par la revue)

### S0 — Correctifs transport & sécurité ✅ (livré le 16/08/2026)
1. `datagram_send_buffer_size(32 Kio)` — le défaut d'1 Mio peut mettre
   **2 minutes de voix** en file sous congestion (bufferbloat).
2. `receive_window(16 Mio)` — le défaut est ILLIMITÉ : un compte authentifié
   peut faire tamponner ~250 Mo au serveur sans qu'il accepte un seul flux.
   **Correctif de sécurité actif, pas une préparation.**
3. `set_priority(+10)` sur le flux de contrôle, des deux côtés.
4. `max_concurrent_uni_streams(256)` des deux côtés.

### S0.5 — Porte de build ✅ GO (livré le 16/08/2026 — openh264 compile en
release + crt-static, aller-retour encode/décode vert, binaire sans CRT
dynamique ; nasm ajouté à la CI ; ⚠ nasm absent en local → encodage ~3× plus
lent en dev, `winget install nasm` recommandé avant S1a)
`openh264` feature `source` dans un crate d'essai : compile en release +
crt-static (le C++ d'openh264 doit passer l'étape dumpbin anti-CRT-dynamique
de la CI), **nasm dans le workflow** (`ilammy/setup-nasm`) — sans nasm,
l'encodage est 3× plus lent, silencieusement. Log au démarrage si le mode
accéléré est absent. Si cette porte casse : on revoit le codec AVANT d'avoir
investi trois sessions.

### S1a — Boucle locale ✅ (livré le 16/08/2026 — mesures sur la machine de
dev : 1920×1080, 26,3 fps capturés = encodés = décodés = peints, **0 sautée**,
conv 1,0 ms + enc 10,2 ms (nasm) + déc 3,2 ms = 14,4 ms/trame ; bouton
« Se voir (test local) » dans ⚙ → 🧪 labo vidéo ; reste à observer pendant
une partie réelle — à faire par l'utilisateur)
Capture WGC → BGRA→I420 → encode → decode → affichage egui **dans le même
processus**. Bouton de test caché : « se voir soi-même », avec
instrumentation par étage (fps capturés / encodés / décodés / **peints**,
ms/frame, débit encodé). Le resize et le plafond de repeint se règlent ici.
**Validation** : 1080p30 affichés (compteur de frames peintes) pendant qu'un
jeu tourne, resize de la fenêtre capturée sans crash ni image cassée.

### S1b — Ça passe sur le fil
Protocole (`KF`/`KA`, `nonce_for_media`, StartStream/Watch/WatchAccepted/…,
`StreamMeta`, `Member.streaming`, `Welcome.features`) ; serveur (table des
streams, ingestion durcie, **tâche par viewer + `mpsc(2)` + needs_idr**,
plafond 32 Mio, cycle de vie complet) ; client (tâche d'émission avec table
des trames en vol, viewer en **panneau de la fenêtre existante**).
**Validation chiffrée** : 1 streamer + 2 viewers en LAN, latence mesurée
(horodatage incrusté) < 150 ms ; gigue vocale p95 et pertes mesurées avant /
pendant le stream : dégradation < 10 ms ; « le streamer tue son client → les
viewers reçoivent `StreamStopped` en < 1 s ».

### S2 — Multi-viewers robuste + UX
`KeyframeRequest` limité, `StreamDegraded`, clause du quantile, stats stream ;
picker avec vignettes (capture one-shot), fenêtre de visionnage **détachée** +
plein écran, badge « diffuse » dans la liste des membres.
**Validation** : un viewer bridé (limiteur de débit) ne dégrade ni les autres
ni la voix, et récupère par IDR.

### S3 — Réseau réel
Ladder adaptatif complet (goodput → `StreamBudget` → encodeur, hystérésis,
`StreamRung`) ; audio du jeu (wasapi EXCLUDE → Opus musique → `KA` → jitter
dédié → sync `pts_us`, réserve vocale 4 Kio, toggle) ; `send_window(1 Mio)`
côté client (mesuré sur le RTT vocal).
**Validation** : WAN réel (Jelastic) + pertes simulées : la vidéo s'adapte,
la voix reste parfaite, écart A/V < 100 ms.

### S4 — Perf & options
Encodage matériel Media Foundation (trait `VideoEncoder`), shader YUV
(`PaintCallback`) pour le 60 fps, intra-refresh (supprime les pics d'IDR),
enveloppes X25519 (niveau 2), qualité manuelle par viewer.

## Risques et parades

| Risque | Parade |
|---|---|
| Jeu plein écran exclusif capturé noir | Fallback capture moniteur (un clic) |
| CPU streamer saturé (jeu + encode) | `skip_frames`, canal capture→encode borné à 1, ladder, S4 matériel |
| IDR 200 Ko = 200 ms sur le fil | Taille d'IDR plafonnée, GOP 2 s, IDR à la demande ; intra-refresh S4 |
| 8 Mbps × 10 viewers = 80 Mbps sortants serveur | Documenté déploiement ; ladder ; plafond streams/salon |
| Windows 10 : bordure jaune, pas de process-loopback (<2004) | Assumé + affiché ; fallback loopback global avec avertissement |
| Deux streamers, priorités croisées | Priorité par-stream relative (même base), test unitaire de saturation |

## Écarté volontairement

Fragmentation sur datagrammes, flux par GOP (MoQ — `group_id` conservé en
en-tête pour garder la porte ouverte), flux long unique, simulcast/SVC, NACK
applicatifs, tampon de GOP serveur, 1080p60 en ladder automatique.
