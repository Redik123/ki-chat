# Plan — la porte web de ki-chat

Un lien à donner à quelqu'un qui n'a ni compte ni l'application :
`https://ts.baws.fun/s/salon1`. Il l'ouvre sur son téléphone, donne un
prénom, un membre l'accepte d'un clic, et le voilà dans un salon textuel
temporaire — et dans le vocal, si on l'y met — depuis son navigateur. À la
fin de la soirée, s'il a aimé, on lui offre ki-chat : un code d'invitation
et le lien de téléchargement, postés dans le salon.

Écrit le 2026-09-21, après trois lectures du dépôt (serveur, client,
sécurité et page web — les trois mémos sont résumés ici), et livré dans
la foulée en 0.1.44. Ce qu'on a vérifié dans le code est marqué
**vérifié** ; les décisions prises avec drion sont dans leur section ; ce
qui reste ouvert est à la fin.

## Ce qu'on veut

- **Zéro installation pour l'invité** : un lien, un prénom, une page qui
  marche sur un téléphone. Pas de compte, pas de mot de passe, rien qui
  survive à l'onglet.
- **Les membres décident** : personne n'entre sans qu'un membre ait vu son
  nom et cliqué « Accepter ». Une bannière avec un son, comme un poke.
- **L'invité ne voit que son salon** : ni la liste des salons, ni celle
  des membres, ni une ligne d'un autre salon, ni une photo de profil.
- **Temporaire, et effacé** : le salon meurt avec la porte ; ce que des
  inconnus ont écrit ne reste pas dans l'historique.
- **Borné et journalisé** : un lien public est une surface d'attaque ; on
  réutilise les gardes du serveur (limiteur, sas, seaux) et tout passe
  dans l'audit.
- **Sans avertissement du navigateur**, à terme : un certificat public sur
  le nom du serveur, sans casser les clients qui épinglent l'auto-signé.
- **Et la voix**, parce qu'une soirée de jeu se passe en vocal : un membre
  amène l'invité dans son salon vocal, et il entend et parle depuis la
  page.

## Ce que ki-chat avait déjà (**vérifié** dans le code)

- **axum compilé avec `ws` depuis le premier commit**, jamais utilisé :
  tokio-tungstenite était déjà dans le `Cargo.lock`. Le transport de la
  page n'a coûté aucune dépendance.
- **Le bus de diffusion travaille sur des lignes JSON pré-sérialisées**
  (`Line = Arc<[u8]>`, `state.rs`) : la même ligne qu'un client QUIC
  reçoit peut être poussée telle quelle dans une WebSocket. Un seul ajout
  au bout de `AppState::broadcast` fait suivre `Chat`, `Reaction`,
  `MessageDeleted`, `MessageEdited` aux invités du salon.
- **`poster_membre`** écrivait déjà un message « au nom de quelqu'un » dans
  l'historique et le diffusait : c'est ainsi que les clips postent. Un
  invité écrit par ce chemin, avec `clean_chat` devant.
- **Créer et supprimer un salon à chaud** existait (`AdminCreateChannel`,
  `AdminDeleteChannel`), avec `history.open_channel` à ne pas oublier.
- **Les gardes** : `Throttle` (cinq essais gratuits, puis 2 s → 60 s
  doublant, oubli après 15 min, table bornée à 4096), le `Sas` par adresse
  (≤ 32 connexions non authentifiées, place rendue au `Drop`), le
  `TokenBucket` du chat, `clean_chat` et `safe_display`, l'audit
  append-only avec ses verbes stables.
- **Les invitations** : codes `ki-…` à usage borné, `create_invite`,
  permission `CREATE_INVITE`, tout tracé.
- **Le serveur détient la clé de session de la voix** (c'est ce qui fait
  marcher le bot musique) : il peut chiffrer et déchiffrer pour quelqu'un
  qui n'a pas de QUIC.
- **`qrcode`** était déjà une dépendance du client (le lien du téléphone
  de l'atelier).
- **Ce qui manquait** : aucun en-tête de sécurité HTTP, aucun limiteur
  par IP sur le chemin HTTP, aucune page HTML servie, et un seul
  certificat — auto-signé, au nom `ki-chat` — épinglé par les clients
  aussi pour le HTTPS.

## Lignes rouges

- **Un invité n'est jamais un `ConnectedUser`.** La table des connectés
  exige une connexion QUIC (la voix, les flux, la déconnexion la lisent) ;
  et `roster()` la transforme entière en membres avec `@everyone`, donc
  tous les salons publics. Les invités vivent dans une table à part, et
  n'entrent dans le reste du serveur que par trois coutures nommées.
- **La porte est le seul chemin d'écriture d'un invité**, et il ne reçoit
  jamais `ChannelsUpdated`, ni `Members`, ni `Avatar`, ni une ligne d'un
  autre salon — texte ou voix.
- **Rien ne s'invente côté protocole** : la page parle le `ServerMsg` du
  client (JSON par ligne, `type` en snake_case) ; tout champ nouveau est
  en `#[serde(default)]`, aucune variante nouvelle dans un enum imbriqué
  (`ChannelKind`), pour qu'un client 0.1.43 ne perde rien.
- **On ne remplace pas le certificat épinglé.** Les clients vérifient
  l'empreinte du certificat QUIC pour leurs téléchargements ; un Let's
  Encrypt sur 8080 casserait uploads, clips et diagnostics de tout le
  monde. Le certificat public est une **seconde** écoute.
- **Le nom d'un invité ne se confond pas avec un membre** : nettoyé comme
  un pseudo, unique, suffixé « (web) » par le serveur — et « (web) » est
  interdit dans un pseudo de compte.
- **Pas d'`innerHTML`** dans la page : tout ce qui vient du réseau passe
  par `textContent`, et la politique de sécurité de contenu n'admet ni
  script en ligne ni gestionnaire en attribut.

## Architecture

```
  navigateur (porte.html + porte.js + porte.css, servis par ki-server)
     │  GET /s/salon1 → la page (404 si la porte n'est pas ouverte)
     │  WS  /s/salon1/ws : {"type":"hello","nom":"Kevin"} puis chat / ping / vocal
     │      trames binaires : [1][compteur u64][Opus] ↑  [1][locuteur][compteur][Opus] ↓
     ▼
  porte.rs ── Portes (un verrou, jamais tenu pendant un appel au reste)
     │   Porte { slug, salon, hote, expire_le, vide_depuis, demandes[], invites[] }
     │   invite_id = INVITE_ID_BASE + n × INVITE_ID_PAS   (2^62, pas de 1024)
     │
     ├─ demande ──▶ send_to(hôte + détenteurs de KICK) : PorteDemande
     │              ◀── PorteRepondre { accepter } depuis ki-chat
     ├─ entrée ───▶ History (50), Info, poster_systeme(« Porte », « Kevin (web) a rejoint »)
     ├─ chat ─────▶ clean_chat + TokenBucket ──▶ poster_membre(salon, invite_id, nom, texte)
     │              ◀── AppState::broadcast(channel, line) ──▶ portes.diffuser(channel, &line)
     ├─ vocal ────▶ Opus de la page, chiffré XChaCha20 sous la clé du serveur, nonce (id, compteur)
     │              ──▶ routes voix ordinaires ; Portes::relayer déchiffre ce que le salon reçoit
     └─ fin ──────▶ Kicked aux invités, delete_and_forget(salon), close_channel,
                    reconcile_memberships, audit porte.close

  ki-chat (porte_ui.rs) ── bouton « Portes » : ouvrir (slug, durée), lien + QR,
     invités (expulser, vocal, offrir), demandes en bannière Accepter / Refuser,
     son « porte », pastille INVITÉ dans le fil et le roster, badge temporaire
     et compte à rebours sur le salon.

  main.rs ── écoute 8080 (certificat auto-signé, épinglé par les clients)
          └─ écoute 8443 ← 443 (KI_TLS_CERT / KI_TLS_KEY, PEM public, relu chaque heure)
             le même Router, les deux écoutes servent tout.
```

### Le serveur (`crates/server/src/porte.rs`, ~2 800 lignes dont ~900 de tests)

- **La table** : `Portes { portes: HashMap<slug, Porte>, ou: invite_id →
  slug, throttle }` sous un seul verrou. Les méthodes rendent ce qu'il faut
  (files d'envoi, noms, salon), et c'est l'appelant, verrou relâché, qui
  prévient — la règle du dépôt, jamais un verrou tenu pendant `send_to`.
- **Le cycle** : `hello` dans les 30 s (sinon fermé) → nom nettoyé
  (`safe_display`, espaces réduits, ≤ `MAX_USERNAME`, suffixe « (web) »
  refusé, noms de comptes, « Musique », le pseudo du fil VALORANT et
  « Porte » refusés) → limiteur par IP (`Throttle`, chaque demande compte
  comme un essai) → plafonds → audit `porte.request` → `PorteDemande` à
  l'hôte et à tout connecté qui a `KICK`, avec l'adresse masquée
  (« 82.65.x.x ») → réponse dans les 5 min, sinon congédié → entré :
  `History`, `Info`, message système, et la ligne du salon lui parvient
  désormais via `broadcast`.
- **Les coutures avec le reste** : la fin de `AppState::broadcast`
  (`portes.diffuser`), le roster (`portes.membres()`, marqués `invite`, pour
  que les membres les voient), `poster_membre` (écrit en leur nom),
  `creer_salon` partagé avec `AdminCreateChannel`, `delete_and_forget`
  dans `channels.rs` (efface le journal au lieu de l'archiver),
  `expire_le` sur `StoredChannel` et `ChannelInfo` (purge au démarrage des
  salons temporaires survivants), le filtre des mentions dans `lus.rs`, et
  `TableauAdmin.portes`.
- **Ce qui borne** : `PORTES_MAX` 5, `PORTE_INVITES_MAX` 20,
  `PORTE_DEMANDES_MAX` 5 et une demande par adresse par porte, le sas par
  adresse dès la poignée de main WebSocket, un `TokenBucket` plus serré
  qu'un membre, une file d'envoi de 128 lignes (un invité qui ne lit plus
  est déconnecté), des trames ≤ 16 Kio, un ping toutes les 20 s et une
  fermeture après 60 s de silence (axum et tungstenite n'ont aucun délai
  d'inactivité, contrairement à QUIC). `PORTE_VIDE_SECS` 10 min sans
  invité, `PORTE_TTL_MAX_SECS` 6 h ; une boucle par minute (`tour`).
- **La page** : `porte.html`, `porte.css`, `porte.js` en `include_str!`,
  servis à part (`/s/porte.css`, `/s/porte.js`) pour une CSP `default-src
  'none'; script-src 'self'` sans `unsafe-inline`, plus `X-Frame-Options:
  DENY`, `Referrer-Policy: no-referrer`, `nosniff`, `no-store`,
  `Permissions-Policy: microphone=(self)`, `Cross-Origin-Opener-Policy`.
  Le nom du serveur y est échappé ; c'est la seule chose injectée.
- **Le vocal** : un invité n'a ni QUIC ni la clé de session. La page
  découpe le micro en trames de 20 ms (AudioWorklet), encode en Opus 48 kHz
  mono (WebCodecs), et envoie `[1][compteur][Opus]`. Le serveur emballe
  exactement comme un client — en-tête voix à son identifiant,
  XChaCha20-Poly1305 sous la clé du serveur, nonce `nonce_for(id,
  compteur)` — et pousse aux pairs du salon par les routes ordinaires : les
  membres l'entendent sans rien savoir de la porte. Dans l'autre sens,
  `Portes::relayer` (appelé par `voice_task` et par le bot musique)
  déchiffre ce que le salon reçoit et le pousse en clair, précédé de
  l'identifiant du locuteur, pour que la page mixe par voix et dise qui
  parle. Être amené dans un vocal (`PorteVocal { invite_id, channel }`, par
  l'hôte ou qui peut expulser, dans **son** salon vocal) n'est qu'une
  autorisation : la page y entre au clic « Rejoindre », dit `vocal
  actif:true`, et c'est ce mot qui le met dans la table d'écoute, dans le
  roster et dans la liste des occupants. Sans WebCodecs (Safari), le texte
  marche et le vocal se déclare indisponible.
- **Les identifiants** : `INVITE_ID_BASE = 1 << 62`, et un pas de 1024
  (`INVITE_ID_PAS`) parce que la page lit les identifiants en doubles
  JavaScript, où seuls les multiples de 1024 s'écrivent exactement entre
  2^62 et 2^63 — sans le pas, tous les invités se confondaient dans la page
  (un seul décodeur pour plusieurs voix, mauvais nom de locuteur). Le
  compteur de la page est strictement croissant (`Date.now() × 50`) pour
  survivre à une reconnexion sans que le serveur voie « un compteur qui
  recule ».
- **L'audit** : `porte.open`, `porte.request`, `porte.accept`,
  `porte.refuse`, `porte.kick`, `porte.close` (motif : hôte, expiration,
  vide), `porte.vocal` — mêmes formes que `invite.use`.
- **La conversion** : `PorteOffrir { invite_id }` (permission
  `CREATE_INVITE`, comme ouvrir : offrir, c'est inviter) crée une invitation
  à usage unique de sept jours, `label « porte salon1 — Kevin »`. Le salon
  reçoit un message système avec `PORTE_TELECHARGEMENT`
  (`https://github.com/Redik123/ki-chat/releases/latest`) et l'adresse à
  saisir (`KI_PUBLIC_QUIC`, sinon l'hôte de `KI_PUBLIC_URL` et le port
  QUIC) — **sans le code** : le salon est lu par tous les invités de la
  porte, des inconnus, et un code d'accès au serveur n'y a pas sa place.
  Le code ne va qu'à la page de l'intéressé (`PorteInvitation`, la carte
  « Installe ki-chat ») et revient à qui l'offre, dans l'`Info` de réponse.

### Le client (`crates/client-gui/src/porte_ui.rs`, ~900 lignes)

- Même patron que le soundboard ou la visionneuse : le module rend, et
  rapporte des `Action`s que `main.rs` applique (envoyer, copier, lire un
  salon, offrir). Jamais de `&mut KiApp` dedans.
- **Le bouton « Portes »** au bas de la barre latérale, visible si le
  serveur a prouvé qu'il gère les portes (`disponible`, posé par le premier
  message de la famille) et qu'on peut créer des invitations — ou qu'une
  porte existe. La fenêtre : slug (normalisé au clavier, validé par
  `slug_valide` des deux côtés), durée (30 min, 1 h, 2 h, 6 h), lien + QR
  (`qrcode`), la liste des invités (expulser, mettre dans mon vocal / l'en
  sortir, offrir ki-chat), les demandes en attente.
- **La bannière** « Kevin veut rejoindre par le web (porte salon1, depuis
  82.65.x.x) — Accepter · Refuser », avec le son « porte » (un carillon,
  deux notes descendantes) et l'attention de la fenêtre ; elle s'efface
  quand quelqu'un d'autre a répondu (`PorteEtat` ne la liste plus) ou
  après cinq minutes.
- **Le salon temporaire** dans la barre latérale avec son compte à rebours
  (`ChannelInfo.expire_le`) et « Fermer la porte » au clic droit ; la
  pastille INVITÉ dans le fil et le roster ; face à un serveur d'avant,
  rien de nouveau ne part et rien ne se montre.

### La page (`porte.html`, `porte.css`, `porte.js`)

- **La fenêtre de ki-chat, en plus léger.** Une grille dont chaque enfant
  a sa place : la colonne des salons à gauche (le badge du serveur, « Salons
  textuels » avec le salon de la porte, « Salons vocaux » avec le vocal
  où l'on a été amené et ses occupants — anneau vert sur qui parle —, et
  soi-même en bas), le fil au centre (avatar à l'initiale, pseudo à la
  couleur que ki-chat lui donne — le même hachage que `theme::color_for`,
  huit teintes —, badge « web » ambré pour les invités), la saisie
  « Message dans #salon » avec son bouton d'envoi, la colonne « En ligne »
  à droite. Sous 1000 px la colonne de droite devient un tiroir, sous
  720 px celle de gauche aussi ; les commandes du vocal passent alors entre
  le fil et la saisie, à portée du pouce.
- **Les présents viennent du serveur** (`porte_presents` : le nom du
  salon, les membres qui le lisent en ce moment, les invités), poussés à
  l'entrée d'un invité et à chaque changement — un membre qui ouvre ou
  quitte le salon (`Join`, `Leave`), qui se déconnecte, un invité qui
  arrive ou part. La page ne devine plus rien aux messages système ; elle
  garde ce repli pour un serveur d'avant.
- **L'adresse à saisir dans ki-chat** (carte « Installe ki-chat ») :
  `KI_PUBLIC_QUIC` si l'admin l'a posée, sinon l'hôte de `KI_PUBLIC_URL`,
  sinon celui par lequel l'invité a ouvert la page — jamais un texte de
  repli.
- **Le lien court** `https://ton-domaine/invite` : la route racine
  `/{slug}` vient après toutes les routes statiques, qui gardent la
  priorité ; `/s/{slug}` reste.

### Le protocole (`crates/protocol`, tout en `#[serde(default)]`)

`ClientMsg` : `PorteOuvrir { slug, nom_salon, ttl_secs }`, `PorteRepondre
{ demande_id, accepter, motif }`, `PorteExpulser { invite_id }`,
`PorteFermer { slug }`, `PorteVocal { invite_id, channel }`, `PorteOffrir {
invite_id }`. `ServerMsg` : `PorteOuverte`, `PorteDemande`, `PorteEtat`,
`PorteInvitation`, `PorteFermee`. Plus `ChannelInfo.expire_le`,
`Member.invite`, `TableauAdmin.portes`, les constantes des plafonds,
`INVITE_ID_BASE` / `INVITE_ID_PAS` / `est_invite`, `INVITE_SUFFIXE`,
`slug_valide`, `PORTE_TELECHARGEMENT`. Un client 0.1.43 jette les variantes
inconnues en silence et voit le salon temporaire comme un salon textuel de
plus.

### Le certificat public (`main.rs`, `deploy/`)

Avec `KI_TLS_CERT` et `KI_TLS_KEY` (PEM), `ecouter_publique` sert le même
`Router` une seconde fois sur `KI_TLS_PORT` (8443 : le conteneur tourne en
utilisateur 10001) via `RustlsConfig::from_pem_file`, et
`reload_from_pem_file` toutes les heures — un renouvellement Let's Encrypt
passe sans redémarrage, un fichier momentanément illisible laisse l'ancien
certificat en service. Fichiers absents au démarrage (un sidecar certbot
met une minute) : nouvel essai toutes les cinq minutes, une ligne en clair
la première fois, puis en debug. Port pris : le journal le dit et le
serveur continue sur 8080. Le compose publie `443:8443` (commenté, prêt),
et le guide décrit deux routes : l'add-on Let's Encrypt de Jelastic, ou un
sidecar certbot dont le `--deploy-hook` copie `fullchain.pem` et
`privkey.pem` dans un volume partagé que ki-chat monte en lecture seule.
Le lien devient `https://ts.baws.fun/s/salon1`, sans port ni
avertissement ; sans certificat, `https://ts.baws.fun:8080/s/salon1` marche
avec l'interstitiel du navigateur.

## Décisions (prises avec drion, 2026-09-21)

- **Slug choisi par l'hôte**, 3 à 24 caractères `[a-z0-9-]` — un lien se
  dicte à voix haute ; pas de tirage au sort. L'énumération des portes est
  acceptée : connaître une porte ne donne rien sans approbation, et les
  demandes sont bornées.
- **Qui approuve** : l'hôte de la porte, et tout connecté qui détient
  `KICK`. Pas de permission nouvelle — « laisser entrer » et « mettre
  dehors » vont ensemble. Ouvrir et offrir exigent `CREATE_INVITE`.
- **Le salon temporaire est effacé**, pas archivé : `delete_and_forget`.
  Il reste l'audit (qui, quand, d'où, combien).
- **Expiration** : 10 min sans invité, 6 h au plus (2 h jusqu'en 0.1.44 : une
  partie avec un invité a duré cinq heures et demie), boucle par minute.
- **Plafonds** : 5 portes, 20 invités et 5 demandes par porte, 1 demande
  par adresse par porte.
- **Identité** : `INVITE_ID_BASE = 1 << 62` (pas de 1024), nom nettoyé
  comme un pseudo, unique par porte (casse ignorée), suffixé « (web) »
  par le serveur — `username = "Kevin (web)"` partout.
- **L'invité n'écrit que par la porte** et ne reçoit jamais
  `ChannelsUpdated`, `Members`, ni rien d'un autre salon.
- **Conversion** : invitation à usage unique de sept jours, postée par le
  serveur dans le salon avec le lien de téléchargement.
- **Routes** : `/s/{slug}` et la forme courte `/{slug}`, après toutes les
  routes statiques, qui gardent la priorité (`/upload`, `/files`, `/diag`,
  `/tel`, `/clips`, `/musique`, `/admin` ne sont pas des portes).
- **TLS** : une seconde écoute, jamais un remplacement ; 443 publié par
  Docker vers 8443 ; certificat par add-on Jelastic ou sidecar certbot,
  relu chaque heure.

## Jalons

### V1 — La porte, texte et voix, et le certificat — livrée (0.1.44, 2026-09-21)

Serveur (`porte.rs`, greffes dans `state.rs`, `channels.rs`, `lus.rs`,
`tableau.rs`, `quic.rs`), protocole, page web (nom → attente → salon →
fermé, refus, reconnexion avec recul exponentiel, carte « Installe
ki-chat », vocal WebCodecs), client (`porte_ui.rs`, son « porte »,
pastilles), seconde écoute TLS, compose et guide, ce plan. Vérifié :
`cargo test -p ki-server` (dont dix tests de `porte::tests` : le cycle
complet, refus / expulsion / offre, les plafonds, l'expiration, la page
et ses en-têtes, le vocal — chiffrement identique à celui d'un membre,
un invité n'entend que son salon, un salon vocal supprimé le sort),
`cargo test -p ki-protocol`, clippy `-D warnings` sur les deux, fumée sur
un vrai serveur (routes, 404 sans porte, priorité des routes statiques,
400 sans poignée de main).

Reste à faire en soirée, sur le serveur du groupe : le certificat
(route A d'abord), un essai depuis un téléphone Android et un iPhone,
et la mesure de la latence vocale affichée par la page.

**Retouches d'après les premiers essais (2026-09-22)** : l'adresse de la
carte « Installe ki-chat » disait « ce serveur:9987 » sans variable
d'environnement — elle reprend l'hôte de la page ; la page a pris la
mise en page de ki-chat (trois colonnes, avatars, présents en direct
par `porte_presents`) ; le lien court `/invite`.

**Le lien exact (0.1.45)** : sans `KI_PUBLIC_URL`, le lien de la porte
n'était que son chemin (« /valo »), et le QR code ne menait nulle part.
L'adresse web publique se règle désormais dans **Admin → Serveur**
(`AdminSetAdresseWeb`, rangée dans `server.json` avec l'identité du
serveur) : `ts.baws.fun:8080` devient `https://ts.baws.fun:8080` —
`normaliser_adresse_web`, la même règle chez le client et le serveur —,
car sans schéma le navigateur tenterait `http://` sur une écoute TLS. Elle
l'emporte sur `KI_PUBLIC_URL`, sert aussi l'atelier des clips, l'origine
admise des WebSockets (l'hôte de la requête reste admis : la page ouverte
par le réseau local garde sa voix) et la CSP de la page. Sans elle, le
client complète le chemin avec l'adresse par laquelle il joint le serveur
(HTTPS, port 8080). `PorteEtat` porte le lien : qui gère la porte sans
l'avoir ouverte le voit, l'hôte le retrouve après une reconnexion, et il
suit l'adresse quand un admin la change. Une durée de **6 h** rejoint les
choix (plafond `PORTE_TTL_MAX_SECS`).

### V2 — Le confort (à décider après la première soirée)

- Répondre à une demande depuis la zone de notification (fenêtre réduite).
- Une porte permanente « accueil », rouverte d'un clic, si le groupe s'en
  sert souvent — avec un slug tiré au sort pour ne pas se faire spammer.
- Réactions et réponses pour l'invité (aujourd'hui : texte seul).
- Le tableau de bord : les portes ouvertes et leurs invités (le champ
  existe, l'écran ne le montre pas encore).
- « Ignorer cette adresse 24 h » sur une demande refusée.

## Risques et parades

| Risque | Parade |
|---|---|
| Spam de demandes → trente bannières | une demande par adresse par porte, cinq par porte, `Throttle` par adresse (chaque demande compte comme un essai), sas par adresse dès la poignée de main ; une demande sans réponse est congédiée à 5 min |
| Pseudo trompeur (« Redik », « (web) Redik », « Musique ») | nom nettoyé, comparé aux comptes, au bot, au fil VALORANT et à « Porte », casse ignorée ; suffixe « (web) » posé par le serveur et interdit aux comptes |
| XSS par un nom ou un message | rendu `textContent` seulement, CSP `default-src 'none'; script-src 'self'`, feuille et script servis à part, nom du serveur échappé |
| Flood, liens d'hameçonnage | `clean_chat` (4000 caractères, contrôles et bidi retirés), `TokenBucket` plus serré qu'un membre, refus sans fermer puis fermeture ; les liens d'un invité restent du texte dans la page |
| WebSockets tenues ouvertes | `hello` sous 30 s, ping 20 s, silence 60 s, file d'envoi bornée, sas par adresse |
| Le certificat auto-signé fait fuir l'invité | seconde écoute avec certificat public (V1) ; sans elle, guider « Avancé → Continuer », Chrome/Android d'abord |
| Firefox / Safari refusent la WSS sur certificat auto-signé même après l'exception | même parade : le 443 public ; à défaut, tester Chrome |
| Un hôte qui ferme ki-chat laisse la porte ouverte | tout détenteur de `KICK` répond et ferme ; expiration 10 min sans invité, 6 h au plus |
| Les messages d'invités sur le disque | le journal du salon est effacé à la fermeture (`delete_and_forget`), pas archivé ; les salons temporaires survivants sont purgés au démarrage |
| Derrière un mandataire, toutes les adresses se ressemblent | pas de mandataire aujourd'hui (IP publique sur le nœud) ; le jour où il y en a un, lire `X-Forwarded-For` **seulement** depuis son adresse |
| Le renouvellement du certificat change l'empreinte | seule l'écoute publique le porte ; les clients épinglent toujours l'auto-signé du QUIC, inchangé |
| La clé privée en 644 dans le volume `ki-chat-tls` | volume monté par deux conteneurs seulement, en lecture seule côté ki-chat ; l'add-on Jelastic évite même cela |

## Questions ouvertes

- **WebTransport plutôt que WebSocket pour la voix ?** WebTransport
  (HTTP/3, datagrammes non fiables) collerait mieux à la voix qu'une
  WebSocket TCP (une trame perdue bloque les suivantes, la latence
  monte). Mais il exige HTTP/3 côté serveur (quinn + h3, une pile de
  plus), un certificat public ou des empreintes `serverCertificateHashes`
  à durée limitée, et Safari ne l'a que récemment. La WebSocket suffit
  pour une soirée ; à revoir si la latence affichée par la page dépasse
  ce qu'on tolère en jeu.
- **Safari.** Le texte marche ; le vocal se déclare indisponible sans
  `AudioEncoder`/`AudioDecoder` Opus (WebCodecs audio est arrivé tard
  chez Apple, et partiellement). Repli possible : encoder en Opus avec un
  WASM (libopus compilé) chargé depuis le serveur — ~300 Kio de plus et
  une dépendance de build. À trancher après un essai sur un iPhone
  réel.
- **ACME intégré** (`rustls-acme`, défi TLS-ALPN-01 sur 443) : zéro
  opération, le serveur obtient et renouvelle lui-même ; mais une grosse
  dépendance, un cache de compte à persister dans `/data`, et un port 443
  qui doit alors être **le serveur** (pas de mandataire devant). On
  commence par l'add-on Jelastic ou le sidecar ; on intègre si l'un des
  deux se révèle pénible à la première rotation.
- **Le HTTPS des clients sur 443, un jour ?** Le client pourrait accepter
  « empreinte épinglée **ou** chaîne WebPKI valide » et basculer ses
  téléchargements sur l'écoute publique ; le 8080 deviendrait facultatif.
  Hors périmètre tant que l'auto-signé ne gêne personne.
- **Une porte permanente**, ou toujours une porte par soirée ? Une porte
  permanente s'énumère et se spamme ; une porte par soirée demande un
  clic. On garde la seconde tant qu'on n'a pas vu la première manquer.
