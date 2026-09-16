# Plan VALORANT — le jeu dans ki-chat

Rédigé le 2026-09-10, après une séance de recherche et de brainstorming.
Objectif : que ki-chat sache **qui joue à quoi, comment ça se passe, et
comment chacun progresse**, sans mettre personne en danger vis-à-vis de
Riot, et sans dépendre d'une clé officielle qu'un groupe privé n'obtiendra
pas.

## Ce qu'on a trouvé

Trois familles de sources, inégales.

**L'API officielle Riot** (`ACCOUNT-V1`, `VAL-CONTENT-V1`, `VAL-MATCH-V1`,
`VAL-RANKED-V1`, `VAL-STATUS-V1`, variantes console). Techniquement propre,
politiquement fermée : pas de clé personnelle pour VALORANT, clé de
développement qui expire toutes les 24 h, clé de production sur candidature
(1 à 3 semaines) avec opt-in RSO obligatoire, et Riot **refuse les
applications à usage privé**. On la garde en réserve ; on ne construit rien
dessus.

**Le client Riot lui-même.** Quand il tourne, il écrit un `lockfile`
(`%LocalAppData%\Riot Games\Riot Client\Config\lockfile`, format
`nom:pid:port:motdepasse:protocole`) et expose sur `https://127.0.0.1:{port}`
(auth Basic `riot:{motdepasse}`) sa session, ses jetons, ses amis et surtout
la **présence** — la sienne et celle de ses amis —, en direct par WebSocket
(`[5, "OnJsonApiEvent_chat_v4_presences"]`). La présence VALORANT est un JSON
en base64 (`private`) : `sessionLoopState` (MENUS / PREGAME / INGAME),
`queueId` (competitive, unrated, deathmatch, spikerush, swiftplay… vide en
partie personnalisée), `matchMap`, `partyOwnerMatchScoreAllyTeam` /
`EnemyTeam`, `partyOwnerMatchCurrentTeam`, `partyState`, `partySize`,
`maxPartySize`, `partyAccessibility`, `competitiveTier`, `leaderboardPosition`,
`accountLevel`, `playerCardId`, `playerTitleId`, `isIdle`, `customGameName`,
`customGameTeam`, `queueEntryTime`, `provisioningFlow`. Avec les jetons
(`/entitlements/v1/token`), on atteint les serveurs que le jeu interroge
(`pd.{shard}.a.pvp.net`, `glz-{region}-1.{shard}.a.pvp.net`) — voir le
catalogue plus bas. Non supporté officiellement (« tant qu'on garde le bon
sens »), fragile à chaque mise à jour, mais sans clé et sans intermédiaire.

**HenrikDev** (`api.henrikdev.xyz`) : le proxy communautaire qui interroge
ces mêmes serveurs Riot avec sa propre infrastructure et rend du JSON propre
— compte, MMR (v3) et historique de RR, historique de matchs (v4) et détails,
matchs archivés, classements, Premier, esports (VLR), contenu, boutique en
vitrine, état des serveurs, files, actualités. Clé gratuite (tableau de bord
après leur Discord) ; palier Basic **30 requêtes par minute** ; non
commercial, mention du service. Un tiers, non approuvé par Riot, qui peut
casser quelques heures quand Riot change quelque chose — mais **aucun compte
de nos joueurs n'est engagé** : c'est notre serveur qui l'appelle.

**valorant-api.com** : toutes les images et données statiques (agents,
cartes, armes et skins, icônes de rang par saison, cartes de joueur, titres,
saisons, modes, version du jeu), `?language=fr-FR`, sans clé.

### La ligne rouge de Riot, en trois interdits

Pas de **scouting** (voir l'équipe adverse avant ou pendant une partie), pas
d'**aide temps réel** en jeu (overlay qui améliore la performance), pas
d'**écriture** par les endpoints non officiels (verrouiller un agent,
rejoindre une party par code) — et jamais de mot de passe Riot demandé à
personne. Ce que Riot accepte : ses propres statistiques, l'historique
personnel, les classements de communauté, avec accord de chacun.

## Architecture

```
  client de Jerem ──lockfile/WS──> présence (état, file, carte, score, party)
          │                               │  opt-in « partager mon activité »
          │  jetons locaux (jamais envoyés)│
          │                               ▼
          │                        ClientMsg::GameStatus ──> serveur ──> Member.jeu ──> tout le monde
          │                                                    │
  Riot ID lié (opt-in) ───────────────────────────────────────>│ data/valorant/comptes.json
                                                               │
                              HenrikDev (clé du groupe, 30/min)│ cache data/valorant/fiches.json
                                                               ▼
                                   fiche joueur, classement, fin de partie, icônes (valorant-api.com)
```

- **La présence vient du joueur lui-même**, en local, quand son client
  tourne. Le serveur ne fait que la relayer, comme l'état vocal. Rien sur le
  disque : fermé le client, elle s'efface.
- **Les statistiques viennent de HenrikDev, côté serveur**, avec la clé du
  groupe — jamais dans l'exécutable du client, jamais dans le dépôt : variable
  d'environnement `KI_HENRIK_KEY` (compose / Portainer) ou fichier
  `data/henrik.key`. Sans clé, tout ce qui en dépend est simplement absent de
  l'interface, sans erreur.
- **Les jetons du client Riot ne quittent jamais la machine** : ils
  permettent d'acheter dans la boutique. Le client ki-chat ne fait que
  **lire**, et n'envoie que des résultats.
- **Opt-in par membre, deux crans** : « partager mon activité Valorant »
  (présence) et « lier mon compte Riot » (Riot ID `Pseudo#TAG`, résolu une
  fois en PUUID + région). Chacun se délie quand il veut ; l'admin peut
  délier ; délier efface tout ce qui en découlait.
- **On ne stocke rien sur les non-membres** : d'un match on garde la ligne
  de nos membres et les scores d'équipe, pas les autres joueurs.

### Budget HenrikDev (30 requêtes par minute)

Un seau à jetons serveur à 20/min (marge pour leurs propres limites), une
file d'attente, et des rythmes bornés :

| Quoi | Quand | Coût |
| :--- | :--- | :--- |
| Lier un compte | à la demande, une fois par membre | 1 (compte) + 3 (la fiche : `v3/mmr`, `v2/mmr-history`, `v4/matches?size=5`) + 2 (rattrapage : `v1/stored-matches?size=60`, `v2/stored-mmr-history?size=100`) = 6 |
| Rafraîchir la fiche d'un membre | toutes les 30 min s'il est connecté, sinon jamais | 3 (`v3/mmr`, `v2/mmr-history`, `v4/matches?size=5`) |
| Fin de partie détectée (présence INGAME → MENUS) | 75 s après, jusqu'à trois fois | 3 (le même rafraîchissement) |
| Ouverture de la page du groupe ou d'une fiche | jamais : servies par le cache | 0 |
| Calendrier esport | toutes les heures | 1 (jusqu'à 7 si la source officielle tombe et que VLR prend le relais) |

Trente membres tous connectés en même temps : trois requêtes par minute en
régime établi, trois par partie finie. Très en dessous des vingt du seau.
Le rattrapage à la liaison est le seul appel aux archives de HenrikDev :
jamais au rafraîchissement, où la fiche accumule d'elle-même.

### Stockage (`data/valorant/`)

- `comptes.json` — liaisons : id ki-chat, Riot ID, PUUID, région,
  plateforme, date. Écriture atomique (renommage), comme `users.json`.
- `fiches.json` — toutes les fiches dans un seul fichier : rang et RR
  courants, pic, actes joués, et depuis 0.1.40 **soixante matchs résumés
  et cent points de RR par membre**, fusionnés à chaque rafraîchissement
  (`MATCHS_GARDES`, `HISTORIQUE_GARDES`). Écrit en JSON lisible (`pretty`)
  ≈ 1,5 Mo à trente membres, réécrit en entier à chaque rafraîchissement —
  sans conséquence sur le VPS, mais à garder en tête.
- `fil.json` — les matchs déjà annoncés (80 ids par membre) ; `recap.json`
  — la date du dernier récap hebdo.
- Pas de base de données : redb, ou un fichier par membre, le jour où l'on
  voudra des années de statistiques (question ouverte).

Côté client : cache disque des images valorant-api.com dans
`%APPDATA%\ki-chat\valorant\`, invalidé quand la version du jeu change.

### Protocole (à ajouter, tous les champs optionnels pour les anciens clients)

- `ClientMsg::GameStatus { jeu: Option<JeuStatut> }` — la présence normalisée
  du membre ; `None` quand il cesse de partager ou ferme le jeu.
- `Member.jeu: Option<JeuStatut>` — relayé à tout le monde, comme `voice`.
- `JeuStatut { etat: Menus | PreGame | EnJeu, file, carte, score_allie,
  score_adverse, party_taille, party_max, party_ouverte, rang, niveau,
  perso_custom }`.
- `ClientMsg::LierRiot { riot_id }` / `DelierRiot`, `ServerMsg::CompteRiot`.
- `ClientMsg::FicheValorant { user_id }`, `ServerMsg::FicheValorant { … }`
  (depuis le cache).
- Une permission ? Non : lier son compte et partager sa présence sont des
  choix personnels ; seul l'admin peut délier quelqu'un d'autre.

### Catalogue des endpoints non officiels utiles (lecture seule)

En-têtes des appels pvp.net : `Authorization: Bearer {accessToken}`,
`X-Riot-Entitlements-JWT {token}`, `X-Riot-ClientVersion` (session locale
`/product-session/v1/external-sessions`), `X-Riot-ClientPlatform` (JSON base64
`platformType: PC, platformOS: Windows, version, chipset`).

- Local : `GET /chat/v1/session` (PUUID, région), `GET /entitlements/v1/token`,
  `GET /chat/v4/presences`, `GET /chat/v4/friends`, WebSocket local.
- pd : `PUT /name-service/v2/players` (PUUID → Pseudo#TAG),
  `GET /mmr/v1/players/{puuid}` (rang par saison, RR),
  `GET /mmr/v1/players/{puuid}/competitiveupdates?queue=competitive`,
  `GET /match-history/v1/history/{puuid}?queue=…`,
  `GET /match-details/v1/matches/{id}`,
  `GET /mmr/v1/leaderboards/affinity/{région}/queue/competitive/season/{saison}`,
  `GET /content-service/v3/content` (saisons, actes),
  `GET /account-xp/v1/players/{puuid}`, `GET /personalization/v2/players/{puuid}/playerloadout`,
  `GET /store/v2/storefront/{puuid}`, `GET /store/v1/wallet/{puuid}` (soi seulement).
- glz : `GET /parties/v1/players/{puuid}`, `GET /parties/v1/parties/{id}`,
  `GET /pregame/v1/players/{puuid}`, `GET /core-game/v1/players/{puuid}`,
  `GET /core-game/v1/matches/{id}` — sans jamais afficher l'équipe adverse.

La V1 n'a besoin que du bloc « local ». Le reste est documenté pour le jour
où l'on voudra se passer de HenrikDev : le client de chaque joueur peut
remonter ses propres statistiques avec ses propres jetons, en lecture.

## Jalons

### V1 — Statut en jeu — livrée en 0.1.31 (2026-09-10)
Livré tel que prévu, à deux écarts près : sondage HTTP toutes les deux
secondes plutôt qu'une WebSocket (plus simple, coût nul), et la case dans
un onglet **Jeu** de ⚙ (le premier habitant de l'onglet ; le Riot ID et le
salon du fil de jeu y viendront). Le statut ne s'affiche encore que dans la
liste des membres — overlay et fenêtre de stream attendent le retour du
terrain. Appris en route : la présence des clients 13.x (2026) est rangée
en blocs (`matchPresenceData`, `partyPresenceData`, `playerPresenceData`),
la lecture accepte les deux dispositions ; au menu, `queueId` n'est que le
mode sélectionné (« en file » seulement si `partyState` = `MATCHMAKING`) ;
les files console portent le préfixe `console_`.

Module client `valorant/` : surveillance du lockfile (apparition,
disparition), WebSocket de présence, normalisation en `JeuStatut`, envoi au
serveur aux changements (et au plus une fois par seconde). Case « Partager
mon activité Valorant » dans ⚙ → Aide & diagnostics ou un nouvel onglet Jeu
— **désactivée par défaut**, avec la phrase qui dit ce qui est lu. Serveur :
champ `Member.jeu` relayé. Interface : sous le pseudo dans la liste des
membres (« compétitive · Ascent · 7-5 »), dans l'overlay (le score de la
party à côté des ronds), dans la fenêtre de visionnage d'un stream.
**Validation** : deux membres en partie, le statut suit à la seconde ; fermer
le jeu efface ; décocher efface chez tout le monde.

### V2 — Identité et fiche joueur — livrée en 0.1.32 (2026-09-10)
« Compte Riot » dans ⚙ → Jeu : on tape « Pseudo#TAG », le serveur le
résout par HenrikDev (`crates/server/src/valorant.rs` : clé lue dans
`KI_HENRIK_KEY` ou `data/henrik.key`, un fil unique, vingt requêtes par
minute glissante, file de travaux) et garde la fiche dans
`data/valorant/fiches.json` — rang courant et pic, dix derniers mouvements
de RR, cinq derniers matchs résumés à la ligne du membre. Le roster porte
`riot_id` et `rang_valorant` ; le pseudo prend son rang en petit, à la
couleur du palier ; clic droit → « Fiche VALORANT » ouvre la fiche depuis
le cache, sans requête. Rafraîchissement toutes les trente minutes pour les
membres liés en ligne. Délier : soi-même, ou un admin (audité). Les
icônes de rang de valorant-api.com sont venues en 0.1.33
(`client-gui/src/rangs.rs`, cache disque, texte coloré en attendant),
avec le palier lu dans la présence pour les non-liés et « Délier son
compte Riot » pour les admins au clic droit.
**Validation** : trente fiches ouvertes en rafale ne coûtent aucune requête
(elles viennent du cache) ; sans clé, la liaison répond « demande à
l'admin » et rien d'autre ne change.

### V3 — Le fil de jeu — livré en 0.1.33 (2026-09-11)
Fin de partie détectée par la présence → relecture de la fiche à 75 s
(jusqu'à trois fois) → message du serveur (pseudo « VALORANT », id 0)
dans le salon choisi par l'admin (`ServerInfo.fil_valorant`,
`AdminSetFilValorant`) : « 🏆 Victoire 13-9 sur Ascent · Compétitif »
puis une ligne par membre du groupe, RR compris en classé
(`PointRR.match_id` fait le lien). Les coéquipiers liés sont reconnus à
leur puuid dans le match, relus aussitôt, attendus deux minutes.
`fil.json` note ce qui a été annoncé ; première fois : l'existant est
réputé connu. Modes d'arcade et matchs de plus de six heures : notés,
pas annoncés. Le classement du groupe est la page Valorant (0.1.33).
L'évolution de la semaine (colonne « 7 jours » de la page Valorant) et la
courbe de RR de la fiche sont venues dans la même version.
**Validation** : une partie finie apparaît en moins de trois minutes,
sans doublon — à vérifier sur une vraie soirée.

### V4 — Confort — en partie (0.1.33 ; récap hebdo le 2026-09-15, commit local)
Livré : « cherche des joueurs » lu dans la party ouverte (affichage
seulement) ; esports par HenrikDev (`/valorant/v1/esports/schedule`, relu
toutes les heures, vingt matchs à venir ou en cours sur la page Valorant) ;
console dès la liaison (HenrikDev `platform=console`) ; compteurs
HenrikDev dans `/diag-resume` ; et la boutique du jour perso
(`client-gui/src/boutique.rs`) : lockfile → session → `/entitlements/v1/token`
→ région `-ares-deployment` → `POST pd.<région>.a.pvp.net/store/v3/storefront/<puuid>`
avec le User-Agent du jeu (sans lui, Cloudflare 1010), noms et images
par valorant-api.com. Sondé le 2026-09-11 : 200, quatre offres. V4 close.

**Récap hebdo (2026-09-15)** : `valorant::recap_de(&fiches, depuis, jusqu_a)`
compte, pour chaque membre lié, les matchs de la semaine (`matchs`, datés),
la somme des `delta` de `historique_rr` dans la semaine, victoires et
défaites, le meilleur match (kills puis score) ; trié par RR gagnés.
`composer_recap` fait le texte ; `Valorant::recap_hebdo()` le rend une
fois par semaine, le dimanche à partir de 19 h 30 UTC (`heure_du_recap`),
la date du dernier dans `data/valorant/recap.json` ; posté dans le fil de
jeu par la boucle de main.rs, avec les annonces. Une semaine sans match ne
se raconte pas.

### V5 — Statistiques enrichies — (0.1.40)

**Livré, protocole** (`crates/protocol/src/lib.rs`). `MatchResume` porte
l'acte, les dégâts infligés et reçus, les tirs à la tête et au total,
l'effectif de sa party, les autres membres du groupe dans son camp
(`avec`) et en face (`contre`), et `manches_detail: Option<DetailManches>`
— la ligne du membre manche par manche : KAST, premiers sangs et
premières morts, triples, quadruples, aces, clutchs tentés et gagnés (et
le plus gros), poses, désamorçages, et le déroulé « VDVV… ». `PointRR`
dit son acte et si la descente a été protégée ; `RangValorant` ses
placements restants, ses boucliers et sa place au classement ; la fiche
liste ses actes (`saisons`). Les agrégats vivent une seule fois, dans le
protocole : `Bilan` (des sommes, les taux en méthodes qui rendent `None`
plutôt que de diviser par zéro), `FicheValorant::bilan()`, `forme()`,
`serie()`, `par_agent()`, `par_carte()`, `duos()`, `resume()` — et
`mmr_estime()`, le MMR caché deviné aux vingt derniers deltas de RR de
l'acte (gagner plus qu'on ne perd : au-dessus du rang), comme le font les
trackers, sans une requête ni une donnée d'autrui. La page du
groupe reçoit par membre un résumé (cinq matchs, dix points) et un
`BilanMembre` calculé à l'envoi, plus les 168 cases `activite` de la
semaine type. Tout nouveau champ a son défaut : une fiche d'avant se
relit, un client d'avant ignore ce qu'il ne connaît pas.

**Livré, serveur** (`crates/server/src/valorant.rs`, `quic.rs`). La fiche
**s'accumule** : `fusionner(ancienne, neuve)` range les cinq matchs frais
sous les anciens (union par `id`, la neuve gagne, tri par date, plafond
`MATCHS_GARDES = 60`) et les points de RR de même (`match_id`, ou
`(date, tier, rr)` s'il manque ; `HISTORIQUE_GARDES = 100`) — sans une
requête de plus, et un `v4/matches` en 429 ne vide plus la fiche. À la
liaison, l'ancienne fiche n'est gardée que pour le même puuid (un joueur
qui se renomme garde son historique), et un **rattrapage** unique lit les
archives de HenrikDev (`stored-matches` size=60, `stored-mmr-history`
size=100 ; `resumer_match_stocke`) sous la fiche fraîche **et** sous la
fiche accumulée (`empiler_a_la_liaison`) : l'archive ne connaît ni les
manches, ni les co-membres, ni la party — elle comble les trous, elle
n'écrase rien de ce qu'on avait détaillé. Les manches se
lisent dans `rounds[]` et `kills[]` déjà téléchargés
(`detailler_manches`, fonction pure : camps comparés sans la casse,
effectifs comptés plutôt que supposés à cinq, kills groupés par manche et
triés par temps, fenêtre d'échange `ECHANGE_MS = 5 000`). `HISTORIQUE_MAX`
passe à 20, `MATCHS_MAX` reste à 5, `ANNONCES_GARDEES` à 80. La réponse à
`StatsValorant` passe par `message_stats` : cinq matchs et dix points par
membre, puis (3, 6), (1, 3), (0, 0) tant que la ligne dépasse
`STATS_MAX_BYTES` — jamais un membre de moins ; testé à quarante fiches
pleines. Le récap hebdo compte enfin la semaine entière, et dit « avec un
ace » quand le meilleur match en a un.

**Livré, client** (`crates/client-gui/src/graphes.rs`, `valo_page.rs`).
Les graphiques au painter (courbe temporelle avec bandes de palier,
sparkline, bande de forme, barres, jauge, heatmap 7 × 24, cases de
manches), la page du groupe en quatre onglets — Groupe (records,
classement triable, duos, heures de jeu), Matchs (le fil de tous, les
parties jouées ensemble regroupées), Esport, Boutique — et une fiche
déroulante avec deux filtres (période, mode). Lisible avec un serveur
d'avant (bilan recalculé localement sur cinq matchs) et avec des fiches
pauvres.

**Ce qu'on ne garde toujours pas.** Rien sur les non-membres : des neuf
autres joueurs d'un match, il ne reste que ce qu'on déduit sur le membre
lui-même — `party: u8` est un **effectif** (combien de joueurs dans sa
party, lui compris), jamais une identité ; `avec` et `contre` sont des
`UserId` du groupe, jamais un puuid ; les `players[]`, `kills[]`,
`rounds[]` sont jetés après lecture. Coût HenrikDev inchangé au
rafraîchissement (3 requêtes) ; la liaison passe de 4 à 6, une fois.

**Validation** : sur deux matchs réels, recouper FK, KAST et clutchs avec
tracker.gg (écart ≤ 1 manche de KAST selon la fenêtre d'échange) ;
vérifier à l'exécution le format de `winning_team` et `killer.team`
(comparés sans la casse ; en cas d'écart, `deroule` vide et clutchs à 0,
jamais une panique) ; lire la taille des lignes `StatsValorant` et le
palier retenu dans le journal en debug (`VALORANT : page du groupe en N
octets`).

## Risques et parades

- Riot change un endpoint ou le format de présence → la fonctionnalité
  s'éteint proprement (journal + « indisponible »), jamais de plantage ; les
  autres fonctions de ki-chat n'en dépendent pas.
- HenrikDev en panne → fiches figées sur le cache, message d'âge.
- Vanguard : lecture locale seulement, pas d'injection, pas d'entrée
  automatisée — rien de ce qu'il traque. Opt-in explicite pour que personne
  ne lance ça sans le savoir.
- Vie privée : rien sur les non-membres, rien sans accord, délier efface.

## Questions ouvertes

- Où mettre la case de présence : onglet « Jeu » dédié dans ⚙ (probable dès
  qu'il y aura aussi le Riot ID et le salon du fil de jeu).
- Le salon du fil de jeu : un salon texte existant choisi par l'admin, ou un
  salon système créé pour l'occasion.
- Faut-il montrer le rang d'un membre qui partage sa présence mais n'a pas
  lié son compte ? La présence porte `competitiveTier` : oui pour l'icône,
  sans fiche.
- `size=10` sur `v4/matches` : la spec ne le borne pas, mais chaque match
  pèse 300 Ko à 1 Mo sous un timeout de vingt secondes ; avec
  l'accumulation, cinq suffisent — à revoir si un membre enchaîne plus de
  cinq parties entre deux relectures.
- Le rattrapage par `stored-matches` / `stored-mmr-history` ne se fait
  qu'à la liaison ; faut-il le rejouer pour les membres liés avant 0.1.40
  (un bouton admin, une fois) ?
- Armes (`weapon.name` nullable), économie (seuil à régler),
  `party_rr_penaltys` (unité inconnue), `session_playtime`, `cluster`,
  `card`/`title`, `elo`/`refunded_rr` de l'historique, `act_wins`, ultis :
  écartés en V5, à reprendre si quelqu'un en veut.
- Pagination de `StatsValorant` au-delà d'une quarantaine de liés : le
  palier (0, 0) tient jusqu'à une centaine, ensuite il faudra découper.
- Un fichier par membre, ou redb, quand `fiches.json` (1,5 Mo réécrit à
  chaque rafraîchissement) deviendra gênant.
- Courbes multi-membres et comparaison de deux fiches (bonus V5.1).
