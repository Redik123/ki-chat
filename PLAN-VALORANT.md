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
                              HenrikDev (clé du groupe, 30/min)│ cache data/valorant/<id>.json
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
| Lier un compte | à la demande | 1 (compte) + 1 (MMR) |
| Rang et RR d'un membre | toutes les 30 min s'il est connecté, sinon jamais | 1 |
| Fin de partie détectée (présence INGAME → MENUS) | ~2 min après | 1 (historique) + 1 (détails) |
| Ouverture d'une fiche | jamais : servie par le cache | 0 |
| Classement du groupe | dérivé du cache | 0 |

Trente membres tous connectés en même temps : une requête par minute en
régime établi, quelques-unes par partie finie. Très en dessous.

### Stockage (`data/valorant/`)

- `comptes.json` — liaisons : id ki-chat, Riot ID, PUUID, région, date,
  drapeaux d'opt-in. Écriture atomique (renommage), comme `users.json`.
- `<id>.json` par membre lié — rang et RR courants, historique de RR, les
  50 derniers matchs résumés (date, carte, mode, agent, K/D/A, score,
  victoire, variation de RR), horodatages de fraîcheur.
- Pas de base de données : quelques centaines de kilo-octets à trente ;
  redb le jour où l'on voudra des années de statistiques.

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

### V2 — Identité et fiche joueur
« Lier mon compte Riot » dans Mon compte ; HenrikDev côté serveur (clé,
seau, file, cache) ; images valorant-api.com en cache ; fiche au clic droit
(rang avec icône, RR, courbe de RR, derniers matchs) ; icône de rang à côté
du pseudo pour qui a lié. **Validation** : trente fiches ouvertes en rafale
ne coûtent aucune requête ; une clé absente ne casse rien.

### V3 — Le fil de jeu
Fin de partie détectée → message automatique dans un salon choisi par
l'admin (« Jerem : victoire 13-9 sur Ascent, Jett 24/12/6, +18 RR ») ;
classement du groupe (rang, RR, évolution de la semaine) dans un onglet ;
historique de RR par membre. **Validation** : une partie finie apparaît en
moins de trois minutes, sans doublon.

### V4 — Confort
« Cherche des joueurs » depuis la party ouverte ; boutique du jour perso
(par le client local, pour soi) ; esports si une source propre existe ;
console (HenrikDev `platform=console`).

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
