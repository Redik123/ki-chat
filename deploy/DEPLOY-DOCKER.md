# Déployer le serveur ki-chat avec Docker / Portainer

Rien à compiler, rien à installer sur l'hôte à part Docker. L'image du
serveur est construite et publiée par GitHub Actions
([`docker.yml`](../.github/workflows/docker.yml)) à chaque poussée sur `main` :

```
ghcr.io/redik123/ki-chat-server:latest
```

Elle existe pour **amd64 et arm64** — un VPS ordinaire, un Raspberry Pi ou un
serveur ARM tirent la même étiquette.

## En bref

1. Ouvre **9987/udp** (et 8080/tcp) sur l'hôte ;
2. Portainer → *Stacks* → *Add stack* → *Web editor*, colle
   [`docker-compose.yml`](docker-compose.yml) ;
3. Ajoute la variable `KI_TOKEN` (ton code d'invitation) et déploie.

Tes joueurs saisissent l'IP publique du serveur dans l'application, leur
pseudo, un mot de passe et ce code. Le **premier compte créé devient admin** —
crée le tien en premier.

## Les ports, et le piège de l'UDP

| Port | Protocole | Rôle |
|------|-----------|------|
| 9987 | **UDP** | QUIC : authentification, chat **et** voix, TLS 1.3 natif |
| 8080 | TCP | HTTPS : téléchargement des fichiers partagés, et les pages des portes web (certificat auto-signé) |
| 443 → 8443 | TCP | *facultatif* — les portes web derrière un certificat public, sans avertissement (voir plus bas) |

Depuis la migration QUIC, **tout passe par 9987/udp**. Ce n'est plus « le chat
marchera, le vocal non » : sans UDP ouvert de bout en bout, personne ne se
connecte du tout. C'est le premier endroit où regarder quand ça ne marche pas.

Chez un hébergeur, il faut donc :

- ouvrir 9987/udp dans le pare-feu du panneau **et** dans celui de la machine
  (`ufw allow 9987/udp`) ;
- une **IP publique attachée au nœud**. Les load balancers partagés (Jelastic
  SLB et consorts) ne font que du TCP.

Le port 8080 reste facultatif : sans lui, tout fonctionne sauf le
téléchargement des fichiers partagés — et les portes web.

## Porte web et certificat public

Une **porte web**, c'est un lien qu'un membre ouvre depuis ki-chat (bouton
« Portes » au bas de la barre latérale) et qu'il donne à quelqu'un qui n'a
ni compte ni l'application : `https://ts.baws.fun/s/salon1`. La page demande un nom, un
membre accepte, et l'invité écrit (et parle, si on l'y met) dans un salon
temporaire depuis son navigateur. Deux variables lui suffisent :

| Nom | Valeur | Rôle |
|-----|--------|------|
| `KI_PUBLIC_URL` | `https://ts.baws.fun` | l'adresse dont le lien est fait (sans barre finale ; avec `:8080` tant que le 443 n'est pas en place) |
| `KI_PUBLIC_QUIC` | `ts.baws.fun:9988` | l'adresse à saisir dans ki-chat, telle qu'une invitation offerte à un invité la donne |

**Sans certificat public, ça marche déjà** : la porte est servie sur
l'écoute 8080, donc `https://ts.baws.fun:8080/s/salon1`. Mais le
certificat de cette écoute est celui du QUIC, auto-signé et au nom de
`ki-chat` : le navigateur affiche « Votre connexion n'est pas privée », et
il faut guider l'invité (« Paramètres avancés → Continuer »). Chrome et
Android s'en sortent ; Firefox et Safari rechignent parfois sur la
WebSocket même après l'exception. Bon pour essayer, pas pour un inconnu.

**On ne remplace pas ce certificat** : les clients ki-chat épinglent son
empreinte pour leurs téléchargements. Le serveur ouvre à la place une
**seconde écoute** HTTPS, du même routeur, derrière un certificat public,
quand `KI_TLS_CERT` et `KI_TLS_KEY` désignent un certificat et sa clé au
format PEM. Elle écoute `KI_TLS_PORT` (8443 : le conteneur tourne en
utilisateur 10001, sans droit sur les ports privilégiés), Docker la publie
sur 443, et les fichiers sont relus chaque heure — un renouvellement passe
sans redémarrage. Le journal dit au démarrage « écoute HTTPS publique
0.0.0.0:8443 avec le certificat … » ; si les fichiers sont illisibles ou le
port pris, il le dit aussi et continue sans, en réessayant toutes les cinq
minutes de lire le certificat. Dans le `docker-compose.yml`, le mappage
`443:8443` est prêt à décommenter.

Deux façons d'obtenir le certificat :

**A. L'add-on « Let's Encrypt Free SSL » de Jelastic.** Sur l'environnement,
*Add-ons* du conteneur ki-chat → *Let's Encrypt Free SSL* → domaine
`ts.baws.fun` → *Install*. L'add-on obtient le certificat (le nœud a une IP
publique, le défi HTTP-01 sur le port 80 passe), le renouvelle tout seul et
dépose les fichiers **dans le conteneur** — le chemin exact est dans le
message de fin d'installation et dans le journal de l'add-on (chez
Jelastic, `/var/lib/jelastic/SSL/fullchain.pem` et `privkey.pem`). Pose
alors dans les variables de la stack :

```
KI_TLS_CERT=/var/lib/jelastic/SSL/fullchain.pem
KI_TLS_KEY=/var/lib/jelastic/SSL/privkey.pem
KI_PUBLIC_URL=https://ts.baws.fun
```

décommente `"443:${KI_TLS_PORT:-8443}/tcp"` dans les ports, redéploie, et
vérifie que l'utilisateur 10001 peut lire les deux fichiers (`docker exec
ki-chat cat /var/lib/jelastic/SSL/privkey.pem >/dev/null`). Deux réserves :
l'add-on installe ses scripts dans le conteneur, qu'une mise à jour
Watchtower recrée — il faut alors le réinstaller (ou le déclencher depuis
le panneau) ; et le chemin peut différer selon la version de la plateforme.
C'est la route sans rien à écrire, à essayer d'abord.

**B. Un sidecar certbot.** Un second conteneur, dans la même stack, obtient
le certificat et le copie dans un volume que ki-chat lit. Le service est
écrit, commenté, dans le `docker-compose.yml` : décommente le service
`certbot`, les volumes `ki-chat-tls` et `ki-chat-letsencrypt`, le montage
`ki-chat-tls:/data/tls:ro` du service ki-chat et le port 443, puis pose :

```
KI_DOMAINE=ts.baws.fun
KI_ACME_MAIL=toi@exemple.fr
KI_TLS_CERT=/data/tls/fullchain.pem
KI_TLS_KEY=/data/tls/privkey.pem
KI_PUBLIC_URL=https://ts.baws.fun
```

Le port **80** doit arriver au sidecar (défi HTTP-01) ; il ne sert à rien
d'autre. Au premier démarrage, certbot obtient le certificat en une minute
et son `--deploy-hook` copie `fullchain.pem` et `privkey.pem` dans le
volume partagé (lisibles par l'utilisateur 10001 : c'est le seul endroit où
la clé est en 644, dans un volume que seuls ces deux conteneurs montent) ;
ki-chat, qui a démarré sans, les trouve à son essai suivant et ouvre
l'écoute — sans redémarrage. Ensuite certbot vérifie deux fois par jour et
renouvelle à trente jours de l'échéance ; ki-chat relit le fichier dans
l'heure. Une mise à jour Watchtower ne touche ni au volume ni au sidecar.

**Le résultat**, dans les deux cas : le lien que ki-chat affiche devient
`https://ts.baws.fun/s/salon1` (ou `https://ts.baws.fun/salon1`), sans port
ni avertissement, et le QUIC 9988 comme le HTTPS 8080 des clients n'ont
pas bougé. Vérification : `curl -sI https://ts.baws.fun/` répond 200 sans
`-k`, et `docker logs ki-chat` montre l'écoute publique.

## Le bot musique

L'image embarque yt-dlp, ffmpeg, ffprobe et deno : le bot musique existe dès que le
serveur démarre (journal : « musique : yt-dlp … · ffmpeg … »). Si YouTube
réclame un compte (« Sign in to confirm you're not a bot »), déposer les
cookies d'un compte Google **jetable** — jamais le principal — au format
Netscape dans le volume : `/data/musique/cookies.txt`. Rien d'autre à
régler ; la permission « Contrôler la musique » se donne dans les rôles.

## Route 1 — Portainer, stack collée (la plus simple)

*Stacks* → *Add stack* → nom `ki-chat` → *Web editor* → colle le contenu de
[`docker-compose.yml`](docker-compose.yml).

Dans **Environment variables**, ajoute au minimum :

| Nom | Valeur |
|-----|--------|
| `KI_TOKEN` | ton code d'invitation (`openssl rand -base64 24`) |

Les autres (`KI_VERSION`, `KI_UDP_PORT`, `KI_HTTP_PORT`, `RUST_LOG`) ont des
valeurs par défaut ; la liste commentée est dans
[`stack.env.example`](stack.env.example).

*Deploy the stack*. Portainer tire l'image, crée le volume et démarre. Si
`KI_TOKEN` manque, le déploiement **échoue volontairement** : mieux vaut une
stack qui refuse de partir qu'un serveur ouvert à tous les vents.

## Route 2 — Portainer branché sur GitHub (mises à jour sans y toucher)

Même écran, mais *Repository* au lieu de *Web editor* :

| Champ | Valeur |
|-------|--------|
| Repository URL | `https://github.com/Redik123/ki-chat` |
| Repository reference | `refs/heads/main` |
| Compose path | `deploy/docker-compose.yml` |

Active **GitOps updates** : Portainer sonde le dépôt (toutes les 5 minutes,
par exemple) et redéploie quand le fichier bouge. Coche également l'option de
**re-tirage de l'image** (*Re-pull image* / *Force redeployment* selon la
version) — sans elle, Portainer ne redéploie que si le `docker-compose.yml`
lui-même a changé, alors que ce qui change le plus souvent, c'est l'image
derrière l'étiquette `latest`.

Pour ne pas attendre le prochain sondage : *Webhook* dans le même panneau,
copie l'URL, range-la dans le secret GitHub `PORTAINER_WEBHOOK`
(*Settings* → *Secrets and variables* → *Actions*). Le workflow appelle ce
webhook après avoir publié l'image — le serveur est à jour quelques secondes
après la fin du build.

Dans cette route, tu peux supprimer le service `watchtower` du compose : c'est
Portainer qui fait le travail, sans avoir besoin de la socket Docker.

## Route 3 — en ligne de commande

```bash
git clone https://github.com/Redik123/ki-chat && cd ki-chat/deploy
cp stack.env.example .env    # puis remplis KI_TOKEN
docker compose up -d
```

Ou sans le dépôt du tout, un seul conteneur :

```bash
docker run -d --name ki-chat --restart unless-stopped \
  -e KI_TOKEN=ton_code_secret \
  -p 9987:9987/udp -p 8080:8080/tcp \
  -v ki-chat-data:/data \
  ghcr.io/redik123/ki-chat-server:latest
```

Mise à jour :

```bash
docker compose pull && docker compose up -d
```

## Si l'image est refusée (`denied` / `unauthorized`)

Un paquet publié sur GHCR est **privé par défaut**. Pour un serveur de jeu
privé, l'image ne contient aucun secret (les comptes vivent dans le volume,
pas dedans) : le plus simple est de la rendre publique, une seule fois.

GitHub → ton profil → *Packages* → `ki-chat-server` → *Package settings* →
*Change visibility* → **Public**.

Sinon, connecte l'hôte au registre avec un jeton d'accès personnel disposant
de la portée `read:packages` :

```bash
echo "$GHCR_TOKEN" | docker login ghcr.io -u Redik123 --password-stdin
```

(Dans Portainer : *Registries* → *Add registry* → *Custom* → `ghcr.io`, mêmes
identifiants. Watchtower, lui, lit le `~/.docker/config.json` de l'hôte.)

## Les mises à jour, en résumé

| Route | Ce que tu fais | Délai |
|-------|----------------|-------|
| Watchtower (livré dans le compose) | rien | ≤ 5 min après le build |
| Portainer GitOps + webhook | rien | quelques secondes |
| Portainer GitOps seul | rien | ≤ l'intervalle de sondage |
| Ligne de commande | `docker compose pull && up -d` | quand tu veux |

Dans tous les cas, la chaîne part de GitHub : `git push` → le workflow
construit et publie l'image → l'hôte la récupère. Le volume de données n'est
jamais touché : les comptes, l'historique et l'identité du serveur survivent
aux mises à jour.

**Épingler une version.** `KI_VERSION=0.1.1` fige la stack sur une version
précise ; `latest` suit `main`. Les tags `v*` publient aussi `0.1` et
`sha-<commit>`, de quoi revenir en arrière sans reconstruire quoi que ce soit.

## Les données

Tout vit dans le volume `/data` : comptes (hachages Argon2id), historique des
salons, fichiers partagés, identité du serveur (nom, logo) et **clé privée
TLS**. Perdre ce volume, c'est perdre les comptes — et changer d'identité aux
yeux des clients déjà installés.

Sauvegarde :

```bash
docker run --rm -v ki-chat-data:/data -v "$PWD":/sauvegarde debian:bookworm-slim \
  tar czf /sauvegarde/ki-chat-data.tgz -C /data .
```

Le conteneur tourne en utilisateur **10001**, pas en root. Si tu remplaces le
volume nommé par un dossier de l'hôte (`-v /srv/ki-chat:/data`), donne-le-lui
d'abord, sinon le serveur ne pourra pas écrire :

```bash
sudo mkdir -p /srv/ki-chat && sudo chown 10001:10001 /srv/ki-chat
```

## Construire l'image soi-même

Depuis la racine du dépôt (le contexte est la racine, pas `deploy/`) :

```bash
docker build -t ki-chat-server -f deploy/Dockerfile .
```

Pour les deux architectures d'un coup, comme le fait la CI :

```bash
docker buildx build --platform linux/amd64,linux/arm64 -f deploy/Dockerfile .
```

La compilation ARM est **croisée**, pas émulée : le Dockerfile installe
`gcc-aarch64-linux-gnu` et cible `aarch64-unknown-linux-gnu` depuis la machine
de build. Quelques minutes, là où un runner émulé en QEMU en demanderait une
heure.

## Dépannage

**Personne ne se connecte (le conteneur, lui, est vert).** UDP. Vérifie que
9987/udp est publié (`docker port ki-chat`) et ouvert dans le pare-feu de
l'hébergeur autant que dans celui de la machine.

**Le conteneur redémarre en boucle.** `docker logs ki-chat`. Le serveur
s'arrête volontairement si son écoute QUIC meurt — sans elle il ne resterait
qu'un serveur de fichiers, sans chat ni voix, mais toujours « en bonne
santé ». Cause la plus fréquente : le port 9987/udp est déjà pris sur l'hôte.

**`unhealthy` dans Portainer.** La sonde interroge le port HTTP interne. Si tu
as changé `KI_HTTP_PORT` dans l'environnement du conteneur (et non le mappage
côté hôte), la sonde suit — mais le conteneur n'écoute alors plus 8080.
Change plutôt le port publié, à gauche du deux-points.

**Un client dit que le certificat a changé.** Le volume a été recréé : le
certificat auto-signé est régénéré au premier démarrage et conservé dans
`/data`. Restaure la sauvegarde, ou préviens tes joueurs.

## Checklist

- [ ] 9987/udp ouvert et testé (connecte-toi vraiment, pas juste `ping`)
- [ ] `KI_TOKEN` long et aléatoire, jamais laissé à `changeme`
- [ ] Volume `ki-chat-data` persistant, et sauvegardé de temps en temps
- [ ] Premier compte créé = **toi** (c'est lui qui devient admin)
- [ ] Une mise à jour vérifiée de bout en bout : pousse un commit, regarde
      l'image arriver
