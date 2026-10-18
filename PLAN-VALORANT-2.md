# Plan VALORANT, saison 2 — les comptes, le tableau de bord, la carte tactique

Rédigé le 2026-10-18 à la demande de drion, après une lecture complète du
code VALORANT (client, serveur, protocole, ki-ui) et des sondages du jour
sur valorant-api.com et sur la spécification de HenrikDev. Il prend la
suite de [`PLAN-VALORANT.md`](PLAN-VALORANT.md) (V1 à V7) et en garde
toutes les lignes rouges. Ce qui a été lu dans le code ou sondé est
marqué **vérifié** ; le reste est à vérifier au jalon qui le concerne.
Les questions pour drion sont à la fin.

## Ce qu'on veut

1. **Savoir sur quel compte Riot un membre joue** : son compte principal,
   un deuxième compte, ou un compte que ki-chat ne connaît pas. Ce n'est
   pas urgent, mais c'est la base des statistiques de demain : chaque
   partie doit être rangée sous le bon compte.
2. **Un nouveau tableau de bord VALORANT** : une page qui donne envie de
   l'ouvrir. On doit y voir d'un coup d'œil qui joue, comment se passe la
   soirée du groupe, où en est son rang. La fiche d'un joueur devient une
   vraie page de profil.
3. **Une carte tactique partagée** : dans un salon vocal, on ouvre la
   carte du match et on dessine dessus à plusieurs (flèches, zones,
   icônes de sorts) pour préparer la manche suivante. **Chaque trait
   s'efface tout seul au bout de 15 secondes.**

## Ce que ki-chat a déjà (**vérifié**)

### Les comptes

- **Un seul compte par membre.** `data/valorant/comptes.json` est un
  `BTreeMap<UserId, CompteRiot>` (`server/valorant.rs`). Lier un compte
  remplace l'ancien.
- **Le client sait sur quel compte il joue, mais ne le dit pas au
  serveur.** `valorant::Session { puuid, game_name, game_tag }` est lu sur
  `/chat/v1/session`, et `Acces.riot_id` en est tiré. Ni l'un ni l'autre
  ne part au serveur. `JeuStatut` n'a aucun champ de compte.
- **Changer de compte sans fermer le client Riot efface le statut.** La
  session n'est relue que quand le lockfile change (`Veilleur::boucle`).
  Sans relecture, le puuid gardé en mémoire ne trouve plus sa présence, et
  le statut reste vide jusqu'au prochain lancement. C'est une déduction du
  code, à reproduire en vrai.
- **Les médailles comparent des chaînes, pas des puuid.**
  `medailles::meme_riot_id` compare des Riot ID. Quand ils diffèrent, le
  journal le dit, et l'interface ne montre rien.
- **HenrikDev est interrogé par `nom/tag`, jamais par puuid** (dans
  `construire`, `lire_medailles` et `rattraper`).
  - Les matchs sont filtrés par puuid (`resumer_match`), mais `v3/mmr` et
    `v2/mmr-history` ne le sont pas.
  - Conséquence 1 : un membre qui renomme son compte n'est jamais suivi.
  - Conséquence 2 : un Riot ID repris par quelqu'un d'autre ferait entrer
    le rang de cet inconnu dans la fiche.
- **Un deuxième compte gâche du budget HenrikDev.** Si une relance ne
  trouve aucun match neuf, elle se reprogramme, jusqu'à trois fois
  (`RELANCES_MAX`). Un membre qui joue une classée sur un autre compte
  coûte donc jusqu'à **9 requêtes pour rien** : le serveur relit trois
  fois son compte principal, où le match n'arrivera jamais.
- **Le badge de rang peut contredire la présence.** Sous le pseudo, le
  badge préfère le rang de la fiche liée (`palier_de`). La présence porte
  pourtant le palier du compte réellement ouvert. Les deux ne sont jamais
  comparés.
- **HenrikDev a des appels par puuid pour tout ce qu'on utilise**
  (spécification `api.henrikdev.xyz/openapi.json`, version 4.6.0, sondée
  ce jour) :
  - `v3/by-puuid/mmr/{affinity}/{platform}/{puuid}`
  - `v2/by-puuid/mmr-history/{affinity}/{platform}/{puuid}`
  - `v4/by-puuid/matches/{affinity}/{platform}/{puuid}`
  - `v1/by-puuid/accolades/…`
  - `v2/by-puuid/account/{puuid}`
  - `v1/by-puuid/stored-matches/…` et `v2/by-puuid/stored-mmr-history/…`
  - **nouveau** : `v1/by-puuid/agent-mastery/{affinity}/{platform}/{puuid}`,
    la maîtrise des agents (piste et niveau par agent) que la V7 attendait
    (« l'Agent Mastery quand son endpoint sera documenté »).

### Les pages

- **Deux fenêtres flottantes.** La page du groupe est une `egui::Window`
  de 860 px (`valo_page_v5`), en quatre onglets : Groupe, Matchs, Esport,
  Boutique. « En direct » n'est pas un onglet, c'est le haut de Groupe.
  La fiche d'un joueur est une seconde fenêtre de 640 px
  (`fiche_valorant_v5`).
- **Défauts visibles :**
  - Les tuiles ont exactement la couleur de la fenêtre (`BG_RAISED` des
    deux côtés, sans bordure) : on ne les voit pas.
  - Les rayures des tableaux (`BG_GHOST` #1a2028 sur #1b212a) ne se voient
    pas non plus.
  - Aucune police grasse n'est chargée : `.strong()` ne fait qu'éclaircir
    le texte.
  - La fiche fait 640 px pour une table de 15 colonnes : elle défile de
    côté.
  - La courbe de RR ne fait que 120 px de haut.
- **Un bug :** dans l'onglet Matchs, les colonnes **KAST et FK affichent
  toujours « — »**. `FicheValorant::resume` retire `manches_detail` de
  tous les matchs envoyés à la page du groupe.
- **Des données déjà calculées mais jamais montrées :**
  - les agents et cartes de `BilanMembre` ;
  - les séries de défaites ;
  - l'`elo` ;
  - la note de performance, le MVP, la taille de party et la durée, qui
    ne sont jamais des colonnes ;
  - les poses et désamorçages ;
  - 6 des 12 médailles dans le tableau du groupe ;
  - les actes (`StatsSaison`), réduits à des pastilles ;
  - le temps de jeu.
- **Le code :**
  - `valo_page.rs` fait 3 700 lignes.
  - Sept fonctions portent `#[allow(clippy::too_many_arguments)]`.
  - Une ligne de match est dessinée deux fois, de deux façons (dans le
    fil et dans la fiche).
  - Les titres de section ont trois styles différents.
  - Les filtres utilisent des `selectable_label` et `ComboBox` bruts au
    lieu de `ui::segmente` / `ui::pastilles`.
  - Ni `Flex`, ni `ui::card`, ni `ui::section` ne sont utilisés.
  - Les lignes du groupe et l'annuaire sont recalculés à chaque image.
- **Ce qui existe et servira :**
  - les graphiques au painter de `graphes.rs` (courbe temporelle à
    paliers, sparkline, forme, barres, jauge, heatmap, cases de manches) ;
  - les jetons de ki-ui (palette, espaces, rayons, tailles de texte —
    19 pt au plus) ;
  - deux bancs d'essai qui photographient l'interface :
    `ui/examples/vitrine.rs` (`VITRINE_CAPTURE`) et
    `client-gui/examples/banc_cartes.rs` (`BANC_CAPTURE`) ;
  - le test `les_fenetres_gardent_leur_largeur_par_defaut`, qui fige les
    largeurs.

### De quoi faire la carte tactique

- **Le jeu de cartes « Dernière carte » est le bon modèle.**
  - Un état par salon vocal :
    `Mutex<HashMap<ChannelId, Salle>>` (`server/cartes.rs`).
  - Les présents sont tenus par `cartes::accorder`, appelé à la fin de
    chaque `rebuild_voice_routes` (arrivée, départ, déplacement,
    déconnexion).
  - La table disparaît quand le salon se vide. Rien n'est écrit sur le
    disque.
  - Un tic de 250 ms fait avancer le temps.
  - Le client ne parle de cartes qu'à un serveur qui annonce
    `Welcome.cartes`.
- **`AppState::broadcast_vocal`** sérialise un message une seule fois et
  l'envoie à tout le salon vocal.
- **Le protocole :**
  - JSON, une ligne par message, sur le flux fiable ; `MAX_LINE` vaut
    160 Kio.
  - Chaque message est rangé dans une classe de coût (`classe_de`). La
    classe `Diffusion` (10 par seconde, rafale de 20) est trop serrée pour
    un dessin en direct.
  - Les datagrammes existent (voix « KV », son du jeu « KA »), mais ils
    sont liés aux routes vocales, à leurs sanctions et à la clé vocale.
- **Les fenêtres :**
  - une `egui::Window` dans l'appli, comme les cartes ;
  - ou sa propre fenêtre système (`show_viewport_deferred`, le modèle de
    `regard_detache.rs`), pour un second écran.
- **Le zoom et le déplacement d'une image** sont déjà faits dans
  `visionneuse.rs` (molette 1-12×, glisser, double-clic). En revanche,
  **aucun dessin à main levée n'existe encore**.
- **Le catalogue (`valo_catalogue.rs`)** ne garde des cartes que leur
  bandeau (`listViewIcon`). Ajouter des champs demande de monter `FORME`
  (2 → 3).
- **`JeuStatut.carte`** dit déjà sur quelle carte joue chaque membre qui
  partage sa présence.
- **valorant-api.com `/v1/maps`** (sondé ce jour, 90 Ko) :
  - `displayIcon` est la minimap : un PNG transparent de 1024×1024,
    27 Ko pour Ascent.
  - `xMultiplier`, `yMultiplier`, `xScalarToAdd` et `yScalarToAdd`
    convertissent les coordonnées du jeu en coordonnées de minimap :
    **u = y·xMultiplier + xScalarToAdd**, **v = x·yMultiplier +
    yScalarToAdd**.
  - `callouts[]` donne les noms des zones (`regionName`,
    `superRegionName`, `location`).
  - **Formule vérifiée sur Ascent** : les 22 zones tombent à leur place
    (A Site en haut à gauche, spawn attaquant à droite, B Boat House en
    bas).
  - Les 13 cartes de compétition ont la minimap et leurs zones. Les cartes
    de TDM ont une minimap, mais sans facteurs (donc sans zones à placer).
  - Les escarmouches, Gauntlet et le stand de tir n'ont pas de minimap :
    on ne les propose pas.
- **valorant-api.com `/v1/agents`** (sondé ce jour) : 29 agents.
  - Chaque agent a son rôle, en français.
  - Chacun a quatre sorts (`Ability1`, `Ability2`, `Grenade`, `Ultimate`)
    avec une icône de 128×128 d'environ 7 Ko : de quoi faire des tampons.
  - Le `displayIcon` d'un agent pèse 568 Ko : on garde le
    `minimapPortrait`.
- **Ni le téléphone (Tauri) ni la porte web** n'ont le jeu de cartes. Le
  téléphone sait déjà entrer en vocal.

## Lignes rouges (en plus de celles de la V1)

- **Un compte non lié ne quitte jamais la machine.**
  - Le client compare lui-même le compte ouvert à ceux que le membre a
    liés.
  - Au serveur, il ne dit que « mon compte lié n° 2 » ou « un autre
    compte ».
  - Jamais le Riot ID ni le puuid d'un compte que le membre n'a pas
    ajouté lui-même.
- **Un deuxième compte est privé tant que son propriétaire ne dit pas
  le contraire.**
  - Privé : il est le seul à voir la fiche de ce compte, et ses parties
    ne sont ni annoncées dans le fil ni comptées dans le récap.
  - Visible : le groupe voit « sur Pseudo2#TAG ».
  - Un compte privé n'entre jamais dans la détection des co-membres :
    sinon, la fiche d'un ami trahirait le second compte.
- **La carte tactique ne lit rien du jeu.**
  - Elle montre ce que les joueurs dessinent, plus le plan et le nom des
    zones venus de valorant-api.com.
  - Aucune position, aucun minuteur de manche, rien sur l'adversaire.
  - C'est la voix, en dessin, comme un tableau blanc.
- **La carte tactique n'entre jamais dans l'overlay en jeu.**
  - La politique développeur de Riot écarte les overlays « qui
    contiennent des données en temps réel qui amélioreraient
    immédiatement la performance d'un joueur », et les overlays
    statiques « qui masquent des parties de la carte ».
  - La carte vit donc dans la fenêtre de ki-chat ou dans sa propre
    fenêtre, sur un second écran.
- **Les traits ne vont jamais sur le disque.**
  - Ils vivent 15 s dans la mémoire du serveur et des clients.
  - Le journal d'audit ne note pas un trait : c'est la règle de la
    musique, on ne journalise pas l'anodin à haute fréquence.
  - Seules l'ouverture et la fermeture d'une carte vont dans
    `tracing::info!`, comme pour les tables de cartes.

---

## V8 — Sur quel compte il joue

### V8.0 — Le ménage, sans rien de visible (taille S)

- **HenrikDev par puuid.**
  - Fonctions concernées : `construire`, `lire_medailles`, `rattraper`.
  - À chaque relecture, `data.account` de `v3/mmr` redonne `nom` et `tag` :
    un compte renommé est suivi de lui-même. `comptes.json` et le roster
    se mettent à jour.
  - Une réponse dont `data.account.puuid` n'est pas celui du compte est
    une erreur, jamais une fiche. C'est vrai aussi pour le MMR.
- **Le changement de compte dans le client Riot.**
  - Si le puuid gardé en mémoire n'a plus de présence VALORANT pendant
    10 s alors que le lockfile n'a pas changé, on relit
    `/chat/v1/session`.
  - À reproduire d'abord en vrai : se déconnecter et se reconnecter sur un
    autre compte sans fermer le client Riot.
- **Les médailles comparent des puuid**, plus des Riot ID.
- **Coût** : aucune requête de plus.
- **Validation** : un test qui relit une fiche d'un compte renommé (nom
  neuf repris, historique gardé) ; un autre où le puuid ne correspond pas
  (erreur, fiche intacte) ; la sonde ignorée (`#[ignore]`) des appels par
  puuid, sur le compte de drion.

### V8.1 — Le compte joué (taille M)

**Le serveur donne au membre ses propres comptes**, et à lui seul :
`ServerMsg::MesComptes { comptes: Vec<MonCompte> }`, envoyé par `send_to`
à la connexion, après une liaison et après un réglage.
`MonCompte { n: u8, riot_id, puuid, principal: bool, visible: bool }` ;
`n` est un numéro stable, que le serveur attribue.

**Le client compare.** Dès que la session Riot est lue, le compte local
est classé, puis le résultat est ajouté à `GameStatus` :

```
JeuStatut.compte: CompteLocal   // #[serde(default)] : Inconnu
enum CompteLocal { Inconnu, Lie { n: u8 }, Autre }
```

**Le serveur vérifie, puis relaie la forme publique.** Il vérifie que `n`
est bien un compte de ce membre (sinon il le traite comme `Autre`). Puis
il pose sur le membre `Member.compte_joue: Option<CompteJoue>` :

```
enum CompteJoue { Principal, Second { riot_id: Option<String> }, Autre }
// riot_id: None quand le compte est privé
```

Le `n` n'est jamais relayé aux autres membres. Un client d'avant
n'envoie rien : le champ reste `Inconnu`, et rien ne change pour lui.

**Ce qu'on voit :**

- **Le membre seul**, quand il joue sur un compte non lié, voit cette
  bannière : « Tu joues sur Smurf#EUW, qui n'est pas lié à ki-chat : ses
  parties ne vont pas dans ta fiche. [Ne plus me le dire pour ce
  compte] ».
  - Avec V8.2 (plusieurs comptes), elle gagne le bouton **[L'ajouter à
    mes comptes]**. Il envoie `LierRiot { riot_id, second: true }` : c'est
    son geste à lui, avec le Riot ID que son propre client a lu.
  - « Ne plus me le dire » garde une empreinte du puuid **sur la
    machine**.
- **Tout le monde**, dans la liste des membres : « compétitive · Ascent ·
  7-5 », puis une petite étiquette « autre compte », ou, avec V8.2,
  « 2ᵉ compte » ou « sur Smurf#EUW ».
  - Le badge de rang suit le compte joué quand il est lié.
  - Sur un compte non lié, le badge prend le palier de la présence, marqué
    « ≈ ».
- **Budget** : une fin de partie sur un compte `Autre` ne relance rien. On
  gagne jusqu'à 9 requêtes par classée.

**Validation** : drion lance VALORANT sur un second compte.
- En moins de 4 s, la bannière s'affiche chez drion seul.
- Les autres voient l'étiquette.
- Le compte principal n'est pas relu à la fin de la partie (on le
  vérifie aux motifs de `/diag-resume`).

### V8.2 — Plusieurs comptes par membre (taille L)

- **`comptes.json` passe à `BTreeMap<UserId, Vec<CompteRiot>>`.**
  - `CompteRiot` gagne `n`, `principal` et `visible`.
  - La lecture accepte l'ancienne forme (`#[serde(untagged)]` : un compte
    seul devient le principal, visible).
  - On garde une copie de l'ancien fichier.
  - **Trois comptes au plus** par membre (`COMPTES_MAX`).
  - Un compte n'appartient qu'à un membre, et ça se vérifie **au puuid**.
    Aujourd'hui, c'est au `nom#tag` sans la casse.
- **Messages :**
  - `LierRiot { riot_id, second: bool }` : `second` vaut faux par défaut,
    et un client d'avant garde le comportement d'aujourd'hui.
  - `DelierRiot { user_id, riot_id: Option<String> }` : `None` délie tout,
    comme aujourd'hui.
  - `ReglerCompte { n, principal: Option<bool>, visible: Option<bool> }`.
  - Un drapeau `Welcome.comptes`, comme pour les cartes.
- **Une fiche par compte, et un fichier par fiche.**
  - Les fiches vont dans `data/valorant/fiches/<puuid>.json` ; seul le
    fichier qui change est réécrit.
  - Cela règle la question ouverte du `fiches.json` de 1,5 Mo réécrit à
    chaque relecture.
  - Au démarrage, la migration découpe l'ancien fichier une seule fois et
    le renomme en `.migre`.
- **La fiche sur le fil.**
  - `ClientMsg::FicheValorant { user_id, compte: Option<u8> }`.
  - La réponse porte la fiche demandée et la liste des onglets de compte
    (Riot ID, principal, rang).
  - Une vue **« Tous les comptes »** que le serveur assemble : les matchs
    visibles mêlés et datés, avec le compte en colonne. Les courbes de RR
    restent séparées, une couleur par compte.
  - On n'envoie jamais trois fiches pleines dans une même ligne, à cause
    de `MAX_LINE`.
- **Le groupe :**
  - Le classement prend une ligne par membre, avec son compte principal
    (voir les questions).
  - Le fil de jeu annonce les parties d'un compte visible, avec « (sur
    Smurf#EUW) ».
  - `Etat::lies` rend tous les comptes visibles, pour la détection des
    co-membres.
- **Réglages ⚙ → Jeu → Comptes Riot** : une ligne par compte (principal ★,
  visible ou privé, délier), et « Ajouter un compte ».

### V8.3 — On relit le compte qu'on joue (taille S)

| Quoi | Quand | Coût |
| :--- | :--- | :--- |
| Rafraîchissement périodique | toutes les 30 min, **seulement le compte vu dans la présence** pendant ces 30 min, sinon le principal | 3 |
| Autres comptes liés, pas joués | une fois par jour si le membre est en ligne | 3 par jour |
| Fin de partie | relance **du compte joué** ; rien pour un compte `Autre` | 3 (0 pour `Autre`) |
| Lier un deuxième compte | une fois | 7, comme une liaison d'aujourd'hui |
| Maîtrise des agents (pour V9) | une fois par jour, pour un compte joué ce jour-là | 1 |

À trente membres, le régime établi reste celui d'aujourd'hui, à une
requête par jour et par compte près. Les soirées sur un second compte
coûtent **moins** qu'aujourd'hui.

### V8.4 — Le journal des parties (taille S, à démarrer tôt)

C'est la réponse à « intéressant pour les stats futures ». Chaque partie
finie que la présence a vue ajoute une ligne à
`data/valorant/parties/<user_id>.jsonl` :

```
{ "debut": …, "duree_s": …, "file": "competitive", "carte": "Ascent",
  "score": [13, 9], "party": 3, "compte": 1 | "autre" }
```

- **Coût HenrikDev : zéro.** Le serveur sait déjà quand une partie
  commence (`en_jeu_depuis`) et comment elle finit
  (`transition_de_jeu`).
- **Ce qu'il apporte :**
  - le temps de jeu par compte, par mode et par jour, avec les durées
    exactes ;
  - les modes que HenrikDev ne garde pas ;
  - les parties sur un compte non lié, comptées sans être identifiées ;
  - les soirées, retrouvées aux écarts entre les parties.
- **Le consentement.** La V1 promettait : « la présence : rien sur le
  disque ». Le journal demande donc **une case à part**, dans ⚙ → Jeu :
  « Garder l'historique de mes parties (durée, mode, carte) ».
  - Décochée, le fichier est effacé.
  - On garde 400 jours et 5 000 lignes au plus.
- **On commence par l'écriture seule.** L'affichage viendra avec V9.4
  (Progression). Chaque semaine sans journal est une semaine de
  statistiques perdue : c'est pour ça qu'il faut le démarrer tôt.

---

## V9 — Le nouveau tableau de bord

### L'idée

On ouvre VALORANT, et l'on sait tout de suite trois choses : **qui joue
en ce moment, comment se passe la soirée, où j'en suis.** Ensuite, on
creuse. La page cesse d'être une fenêtre flottante de 860 px. **Elle prend
la place du chat**, en gardant la liste des salons et celle des membres
(Échap, ou un clic sur un salon, la referme). Un bouton **⤢ détacher**
l'envoie dans sa propre fenêtre, pour un second écran (sur le modèle de
`regard_detache`).

```
┌─ VALORANT ─────────────────────────────────────────────────────── ⤢  ✕ ┐
│ Accueil · Classement · Matchs · Profil · Tactique · Esport · Boutique  │
├────────────────────────────────────────────────────────────────────────┤
│ EN DIRECT  [Ascent ▓▓ 7-5 · compétitive · drion, Jerem]  [Lotus 2-1]   │
│                                                                        │
│ CE SOIR  4 V – 1 D · +61 RR à nous tous · MVP : Jerem (ACS 287)        │
│                                                                        │
│ ┌ MOI ────────────────┐ ┌ K/D ──┐ ┌ ACS ──┐ ┌ KAST ─┐ ┌ VICT. ─┐       │
│ │ ◆ Platine 2 · 64 RR │ │ 1,21  │ │ 231   │ │ 74 %  │ │ 58 %   │       │
│ │ ▓▓▓▓▓▓░░░ → Plat. 3 │ │ ▲0,08 │ │ ▼12   │ │ ▲3    │ │ ▲5     │       │
│ │ MMR ≈ Plat. 3 (+)   │ │ ~~~~  │ │ ~~~~  │ │ ~~~~  │ │ ~~~~   │       │
│ └─────────────────────┘ └───────┘ └───────┘ └───────┘ └────────┘       │
│                                                                        │
│ PODIUM DE LA SEMAINE   1. Jerem +142   2. drion +88   3. Max +40       │
│ RECORDS · DUOS · QUAND LE GROUPE JOUE                                  │
└────────────────────────────────────────────────────────────────────────┘
```

### V9.0 — Les fondations (taille M) — à faire d'abord, et une seule fois

**Dans ki-ui, au service de toutes les pages et pas seulement de
VALORANT :**

- **Jetons :**
  - des surfaces qui se détachent : une carte en `BG_RAISED` posée sur une
    page `BG_BASE`, avec une bordure `BORDER_SOFT` ;
  - des jetons d'élévation (`ombre::douce`, `ombre::levee`), pris aux
    valeurs aujourd'hui codées en dur dans `style.rs` et
    `composants.rs` ;
  - `texte::KPI = 28` et `texte::TITRE_PAGE = 22`.
- **Une vraie police grasse** : une variante grasse de la police
  d'interface, sous licence libre, environ 300 Ko dans l'exécutable. Ainsi
  `.strong()` sera enfin gras.
- **`ui::kpi`** : une tuile avec un libellé, une grande valeur, un écart
  coloré (▲ ▼) et la place pour une sparkline.
- **`ui::Tableau`** : un tableau de données décrit par ses colonnes.
  - Tri, et colonnes masquées par priorité quand la largeur manque.
  - Rayures visibles, survol de ligne, clic.
  - Il remplace les cinq `egui::Grid` faits à la main, et la ligne de
    match dessinée deux fois.
- **Une grille de cartes qui passe à la ligne**, sur `Flex`, avec des
  points de rupture rangés dans les jetons.

**Dans `graphes.rs` :**

- une courbe à plusieurs séries, avec sa légende (les comptes de V8,
  la comparaison de V9.4) ;
- des valeurs sur l'axe des y ;
- un anneau (le pourcentage de victoires) ;
- des barres verticales (un acte par barre) ;
- des valeurs qui glissent en 250 ms (`ctx.animate_value_with_time`).

**Le code de la page :**

- `valo_page.rs` est découpé en `valo/{mod, accueil, classement, matchs,
  profil, comparer, composants}.rs`.
- Un `Contexte` regroupe stats, membres, rangs et catalogue : les sept
  `too_many_arguments` disparaissent.
- Les vues dérivées (lignes du groupe, annuaire) sont recalculées quand un
  message arrive, plus à chaque image.

**Le catalogue** (`FORME` monte d'un cran) : le rôle des agents et les
icônes de rang en grand (`largeIcon`). Les minimaps et les icônes de
sorts y sont déjà, venues avec V10.1.

**Le serveur :**

- **Le bug KAST/FK.** `resume()` garde désormais pour chaque match un
  « détail léger » (KAST, premiers sangs, premières morts), à peu près
  40 octets de plus par match. Les colonnes de l'onglet Matchs se
  remplissent enfin.

**Le banc d'essai :**

- `client-gui/examples/banc_valo.rs` photographie chaque page sur des
  fiches d'essai (pleines, pauvres, vides, un serveur d'avant), comme
  `banc_cartes`. Ces images servent à relire chaque jalon. Le test des
  largeurs est mis à jour.
- La nouvelle page arrive d'abord dans ⚙ → **Bêta** pour une version, à
  côté de l'ancienne, puis la remplace.

### V9.1 — Le profil (taille L)

La fiche devient une page.

- **En-tête :**
  - la carte de joueur en grand, et l'avatar ;
  - le Riot ID, le niveau et le titre ;
  - le rang en grand, avec la jauge vers le palier suivant, le pic et le
    MMR estimé ;
  - « encore ≈ 2 victoires pour Platine 3 », estimé d'après le gain moyen
    des victoires récentes ;
  - le choix du compte (V8.2 : Principal · Smurf#EUW · Tous).
- **Onglets :**
  - **Vue d'ensemble** :
    - les tuiles KPI : K/D, ACS, ADR, KAST, Tête, Victoires, premiers
      sangs, clutchs ; chacune avec son écart par rapport aux dix matchs
      d'avant, et son repère médian du groupe ;
    - la courbe de RR, 220 px de haut, avec ses valeurs ;
    - la forme et la série en cours (défaites comprises) ;
    - les trois agents favoris en cartes (portrait, rôle, parties,
      victoires, K/D, niveau de maîtrise) ;
    - les cartes jouées, sur leur bandeau, avec le pourcentage de
      victoires.
  - **Matchs** : la table (`ui::Tableau`), avec en colonnes la note, le
    MVP, la party et la durée. Une ligne dépliée montre les manches et le
    récit.
  - **Agents** : la grille de tous les agents joués.
  - **Cartes** : la même grille, avec le partage **attaque / défense**
    (voir plus bas).
  - **Médailles** : les douze, l'acte en cours et la carrière, les
    records ★.
- **Attaque et défense, sans requête de plus.** On ajoute
  `DetailManches.attaque_d_abord`, lu dans le match déjà téléchargé (le
  camp du membre, comparé à celui qui attaque en première mi-temps). Avec
  le déroulé « VDVV… », on obtient le pourcentage de manches gagnées en
  attaque et en défense, par carte. La règle qui le permet (l'équipe Red
  attaque en première mi-temps) est à vérifier sur deux matchs réels.

### V9.2 — L'accueil et la soirée (taille M)

- **En direct, en cartes** :
  - le bandeau de la carte en fond, le score en grand, le mode ;
  - les avatars des membres de la même partie ;
  - « cherche des joueurs » quand la party est ouverte.
- **Ce soir.**
  - Nouveau message `ClientMsg::SoireeValorant { depuis }`, avec sa
    réponse `ServerMsg::Soiree { matchs }`.
  - `depuis` est le dernier 6 h du matin à l'heure du client : le serveur
    compte en UTC. Une partie à 1 h du matin reste donc dans la soirée de
    la veille.
  - La réponse contient tous les matchs du groupe depuis cette heure, un
    match joué ensemble ne comptant qu'une fois.
  - On en tire les victoires et défaites du groupe, la somme des RR, le
    MVP de la soirée et le meilleur duo.
  - C'est un message à part, pour ne pas alourdir `StatsValorant`, qui
    plafonne déjà à cinq matchs par membre.
- **Moi** : mon rang, ma jauge, ma journée, mes tuiles KPI.
- **Podium de la semaine** (les RR gagnés en 7 jours), **records**,
  **duos** et **heures du groupe**, mis en cartes.

### V9.3 — Classement et Matchs (taille M)

- **Le classement :**
  - passe sur `ui::Tableau`, avec avatar, icône de rang, sparkline,
    forme et 7 jours ;
  - les colonnes se masquent par priorité selon la largeur ;
  - une case « comparer » sur chaque ligne.
- **Les matchs deviennent des cartes :**
  - le bandeau de la carte à gauche, et un liseré de couleur pour le
    résultat ;
  - pour chaque membre du groupe : une pastille avec le portrait de son
    agent, son K/D/A, son ΔRR et ses médailles ;
  - une partie jouée ensemble fait une seule carte ;
  - les filtres passent sur `ui::pastilles`.

### V9.4 — Comparer et Progression (taille M)

- **Comparer deux membres** (c'était le bonus V5.1) : tuiles côte à côte,
  courbes de RR superposées, agents et cartes en commun, bilan quand ils
  jouent ensemble.
- **Progression :**
  - le rang de fin d'acte, en barres ;
  - la courbe de RR sur toute l'histoire gardée ;
  - le MMR estimé ;
  - le temps de jeu par semaine, par mode et par compte, tiré du journal
    de V8.4.

**Validation V9** : à chaque jalon, les captures du banc pour des fiches
pleines, pauvres et vides. On lit la page à 720 px et à 1 600 px de
large. Pas de défilement horizontal dans le profil à 900 px. Le journal
de debug ne montre aucune réorganisation en boucle (le mouchard de
ki-ui). Un serveur d'avant donne une page lisible.

Hors périmètre : un thème clair. Toute l'appli peint des constantes de
couleur ; c'est un chantier à lui seul.

---

## V10 — La carte tactique

### Comment on s'en sert

- **Ouvrir la carte.**
  - Dans un salon vocal, le bouton **« Tactique »** de la barre vocale, à
    côté de « Cartes » (le jeu de cartes).
  - Ou depuis la page VALORANT, sur la ligne « En direct » de sa partie.
  - Celui qui ouvre la carte l'ouvre **pour tout le salon**. Les autres
    voient une bannière : « drion a ouvert la carte tactique — Ascent
    [Voir] ».
- **Le plan se choisit tout seul** : celui de la carte que joue celui qui
  ouvre (d'après sa présence), sinon celle que jouent la plupart des
  membres du salon, sinon un choix parmi les 13 cartes de compétition,
  chacune avec sa minimap. Changer de plan efface tous les traits, chez
  tout le monde.
- **Attaque / Défense** fait pivoter la vue de 180°, chacun chez soi. Les
  traits sont rangés dans le repère de la carte et se voient à leur place
  quelle que soit la vue.
- **Les outils** (touches 1 à 6) :
  1. **Crayon** : un trait libre, lissé.
  2. **Flèche** : un chemin terminé par une pointe (une poussée, une
     rotation).
  3. **Zone** : un cercle tiré au glisser (un site, une fumée).
  4. **Icône** : le portrait d'un agent, ou l'un de ses quatre sorts
     (fumée, flash, molly, mur). Les cinq agents de la party d'abord.
  5. **Laser** (clic droit maintenu) : une traînée qui s'efface en une
     seconde, pour dire « regardez ici ». Rien n'est gardé.
  6. **Ping** (double-clic) : une onde, et un petit son chez tout le
     salon.
  - **Ctrl+Z** annule son dernier trait. « Effacer les miens » vide ses
    propres traits.
- **Une couleur par joueur** : celle de son pseudo (`pour_pseudo`). Pas de
  nuancier ; on sait qui a dessiné quoi. Le pseudo s'affiche une seconde
  au début du trait.
- **Les noms des zones** (« A Main », « Mid Courtyard ») se montrent ou se
  cachent d'une touche.
- **15 secondes.**
  - Un trait vit 15 s à partir du moment où l'on lève le crayon : 12 s
    plein, puis 3 s de fondu.
  - Il ne vieillit pas tant qu'on le trace.
  - Qui arrive en cours de route voit les traits encore vivants, avec le
    temps qui leur reste.
- **Molette** pour zoomer (de 1× à 4×), glisser au clic du milieu pour
  déplacer, double-clic dans le vide pour recadrer (le modèle de la
  visionneuse).
- **⤢ Détacher** pose la carte dans sa propre fenêtre, sur un second
  écran, avec F11 pour le plein écran. Le jeu reste devant, et ki-chat
  sur l'autre écran.

### Le fil

On envoie des **traits par morceaux**, pas des instantanés. La table de
cartes renvoie toute la table à chaque coup ; vingt instantanés par
seconde de tous les traits seraient trop lourds.

```
ClientMsg::Tactique { action: ActionTactique }
  Ouvrir { carte } · Quitter · ChoisirCarte { carte }
  Trait { id: u32, outil, points: Vec<[u16; 2]>, fin: bool }
  Icone { id: u32, quoi: IconeTactique, pos: [u16; 2] }
  Laser { pos: Option<[u16; 2]> } · Ping { pos: [u16; 2] }
  Annuler { id: u32 } · EffacerLesMiens

ServerMsg::Tactique { evenement: EvenementTactique }     // à tout le salon
ServerMsg::TactiqueEtat { carte: Option<CarteTactique> } // à l'arrivée
```

- **Le repère.** Les points sont rangés dans le repère de la minimap,
  de 0 à 65 535 sur chaque axe. Le plan et le zoom de chacun n'y changent
  rien.
- **Le rythme.** Le client groupe ses points et les envoie toutes les
  50 ms pendant le tracé. Il saute les points à moins de 2 px du
  précédent. `fin: true` lance les 15 s.
- **Le serveur fait autorité :**
  - L'auteur est la connexion qui envoie : on ne croit jamais un auteur
    écrit par le client. Un trait se reconnaît à `(auteur, id)`.
  - L'âge vient du serveur, qui envoie aussi `reste_ms` pour les traits
    vivants.
  - Un tic, celui des cartes ou le sien à 250 ms, jette les traits morts.
- **Diffusion** : `broadcast_vocal`, qui sérialise une fois et envoie au
  salon.
- **Bornes** (à figer dans le protocole, testées comme celles des
  cartes) :
  - 512 points par morceau, 4 000 points par trait ;
  - 30 traits vivants et 12 icônes par joueur ;
  - le laser à 20 par seconde au plus (le serveur fusionne au-delà).
  - Une nouvelle classe `Classe::Dessin` (30 par seconde, rafale de 60),
    à part de `Diffusion`, pour que dessiner ne prive pas du chat.
  - Un morceau plein pèse environ 7 Ko, loin de `MAX_LINE`.
  - Un joueur qui dessine envoie à peu près 3,5 Ko/s (vingt morceaux de
    six points). Cinq en même temps, c'est une quinzaine de Ko/s reçus par
    chacun : négligeable.
- **Pas de datagrammes** : un morceau perdu casserait le trait, et les
  datagrammes sont liés aux routes et aux sanctions vocales. Le laser
  pourra y passer plus tard, si le flux fiable ne suffit pas.
- **Qui peut dessiner** : quiconque est dans le salon vocal, comme pour
  les cartes. Un membre rendu muet par un modérateur peut regarder, pas
  dessiner (voir les questions).
- **Durée de vie** : un crochet dans `rebuild_voice_routes`, comme
  `cartes::accorder`. Qui quitte le salon quitte la carte, et ses traits
  s'éteignent. Un salon vide efface la carte.
- **Compatibilité** : un drapeau `Welcome.tactique`, des champs en
  `#[serde(default)]`, et `PROTOCOLE` ne bouge pas.

### Le client

- **Le module.** `client-gui/src/tactique.rs`, calqué sur
  `cartes.rs` : un état, `recevoir()`, et `contenu(ui)` à part de la
  fenêtre, pour servir dans la fenêtre de l'appli comme dans la fenêtre
  détachée.
- **Le dessin :**
  - la minimap du catalogue, puis les traits en `Shape::line` aux bouts
    ronds, lissés à l'affichage seulement (Catmull-Rom) ;
  - une épaisseur qui suit le zoom, une pointe pour les flèches ;
  - le fondu : `alpha = clamp((15 − âge) / 3, 0, 1)`, adouci ;
  - les icônes de sorts en 128 px.
- **Les repeints** : `request_repaint_after(33 ms)` tant qu'un trait est
  vivant ou un laser actif, puis rien. Une carte vide ne coûte rien.
- **Les images** : les 13 minimaps (environ 30 Ko chacune) se
  téléchargent quand on ouvre la carte, puis restent en cache disque.
- **Le banc d'essai** : `examples/banc_tactique.rs` photographie Ascent
  avec des traits de plusieurs âges, de plusieurs auteurs, en attaque et
  en défense.

### Les jalons

- **V10.1 — La carte, seul (taille M)**. Tout côté client, sans serveur.
  - Le catalogue apprend les minimaps, leurs facteurs, leurs zones et les
    icônes de sorts (`FORME` passe de 2 à 3).
  - Le choix du plan, la minimap, les noms des zones, le crayon, la flèche
    et la zone, le fondu de 15 s, le zoom.
  - Ça sert à **régler la sensation** (épaisseur, lissage, fondu) avant de
    toucher au réseau.
- **V10.2 — La carte partagée (taille M)**. Le protocole, le serveur, la
  synchronisation, l'arrivée en cours de route et les bornes. Des tests
  serveur comme ceux des cartes : expiration, bornes, auteur forcé,
  départ du salon, salon vide.
- **V10.3 — Le confort (taille M)** : les icônes d'agents et de sorts, le
  laser, le ping et son son (`sfxgen`), la fenêtre détachée, la bannière
  « X a ouvert la carte » et la carte choisie d'après la présence.
- **V10.4 — Le téléphone (en option)**. L'appli Tauri entre déjà en
  vocal ; la carte s'y dessine sur un `<canvas>`, au doigt. Le téléphone
  devient le second écran de ceux qui n'en ont pas.

**Validation V10** : une vraie soirée à cinq dans un salon vocal.
- Un trait apparaît chez les autres en moins de 150 ms et s'éteint au
  même instant chez tous (à une demi-seconde près).
- Un retardataire voit les traits vivants.
- Une rafale de dessin ne ralentit ni le chat ni la voix.
- Le jeu au premier plan ne perd pas d'images quand la carte est ouverte
  sur l'autre écran.

---

## L'ordre proposé

| # | Jalon | Pourquoi à ce moment |
| :--- | :--- | :--- |
| 1 | **V8.0 + V8.1 + V8.4** | Petit, et ça corrige trois vrais défauts : le renommage, le compte confondu, les 9 requêtes perdues. Le journal commence à compter tôt. |
| 2 | **V10.1 + V10.2** | Indépendant du reste, et c'est ce qui se voit à la prochaine soirée. Le catalogue des minimaps servira aussi au tableau de bord. |
| 3 | **V9.0** | Les fondations de ki-ui. Toutes les pages en profitent. |
| 4 | **V9.1 → V9.2 → V9.3** | Le profil d'abord : c'est la page la plus ouverte. |
| 5 | **V8.2 + V8.3** | Plusieurs comptes, maintenant que le profil a sa place pour les onglets de compte. |
| 6 | **V10.3, V9.4** | Le confort de la carte, la comparaison et la progression (le journal a alors quelques semaines). |
| 7 | V10.4 | Le téléphone, si l'envie est là. |

Chaque jalon fait une version, avec sa ligne dans le README et sa
validation en soirée, comme d'habitude.

## Risques et parades

- **Riot change la session ou la présence.** Le compte joué reste
  `Inconnu` et rien d'autre ne casse. Le journal note « inconnu ».
- **Les appels par puuid de HenrikDev se comportent autrement**
  (champs, coût, erreurs). Une sonde ignorée les vérifie d'abord sur le
  compte de drion. Le chemin par `nom/tag` reste en secours un temps.
- **La migration de `comptes.json` et des fiches.** Copie de l'ancien
  fichier, migration qu'on peut rejouer sans dégât, testée sur une copie
  du dossier `data/` de production avant la mise en ligne.
- **La carte sert à spammer.** Les bornes et la classe `Dessin`. Et
  tout s'efface en 15 s, ce qui limite d'office les dégâts.
- **Une carte sort sans minimap ou sans zones.** Elle est proposée sans
  plan (fond quadrillé) ou sans noms. La liste des cartes vient du
  catalogue, pas d'une table en dur.
- **valorant-api.com tombe.** Le cache disque garde les minimaps déjà
  vues.
- **Le tableau de bord régresse quelque part.** Le banc d'essai à
  chaque jalon, et la nouvelle page passe une version en ⚙ → Bêta.
- **La politique de Riot.** Aucune donnée de jeu sur la carte tactique,
  pas d'overlay. Les statistiques restent celles du membre, avec son
  accord, comme depuis la V1.

## Questions pour drion

1. **Seconds comptes** : privés tant que leur propriétaire ne dit pas le
   contraire (ma recommandation), ou visibles d'office ?
2. **« Sur un autre compte »** : quand quelqu'un joue sur un compte non lié
   ou privé, le groupe voit-il cette étiquette, sans nom ? Ma
   recommandation : oui. Sinon, le badge de rang ment (Immortel affiché
   dans une partie Argent).
3. **Le journal des parties** : case décochée, que l'on propose une fois
   par une bannière (ma recommandation, fidèle à la promesse de la V1),
   ou cochée d'office ?
4. **Le classement** : par compte principal (ma recommandation), ou par
   meilleur compte ?
5. **La page VALORANT** : elle prend la place du chat, avec un bouton pour
   la détacher (ma recommandation), ou elle reste une fenêtre flottante,
   en plus grand ?
6. **La carte tactique** : 15 s pour tout, ou aussi une épingle pour
   garder un trait plus longtemps (un plan de base qu'on reprend à chaque
   manche) ?
7. **L'overlay** : rien du tout (ma recommandation), ou une seule ligne
   de texte « drion dessine sur la carte » ?
8. **Un membre rendu muet** par un modérateur : peut-il dessiner ? Ma
   recommandation : non, le dessin est une parole.

Sources consultées le 2026-10-18 :
[politique développeur VALORANT de Riot](https://developer.riotgames.com/docs/valorant),
[spécification HenrikDev 4.6.0](https://api.henrikdev.xyz/openapi.json),
[valorant-api.com `/v1/maps` et `/v1/agents`](https://valorant-api.com).
