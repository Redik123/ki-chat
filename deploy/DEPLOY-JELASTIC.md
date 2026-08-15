# Déployer ki-chat sur Jelastic

## ⚠️ Le point crucial : l'UDP

La voix transite en **UDP sur le port 9987**. Le load balancer partagé de
Jelastic (SLB) et les « endpoints » classiques sont pensés pour le TCP :
**attache une IP publique (IPv4) au nœud** dans la topologie de
l'environnement — c'est la seule façon fiable d'avoir l'UDP qui passe.
Sans ça : le chat marchera, le vocal non.

Ports à ouvrir (firewall Jelastic du nœud) :

| Port | Protocole | Rôle |
|------|-----------|------|
| 9987 | UDP | QUIC : contrôle + voix, TLS 1.3 natif |
| 8080 | TCP | HTTP : partage de fichiers uniquement |
| 443  | TCP | optionnel, reverse proxy devant le 8080 pour des liens https |

Depuis la migration QUIC, tout le trafic temps réel (auth, chat, voix) passe
sur le seul port 9987/udp, déjà chiffré — aucun reverse proxy nécessaire.

## Route A — Elastic VPS (recommandée, la plus simple)

1. Crée un nœud **Elastic VPS** (Ubuntu 22.04+) avec IP publique.
2. Compile le serveur sur le nœud (2 min, il est léger — aucune dépendance
   audio ni C) :

   ```bash
   apt update && apt install -y build-essential curl
   curl --proto '=https' -sSf https://sh.rustup.rs | sh -s -- -y
   source "$HOME/.cargo/env"
   # dépose les sources (zip du projet sans target/) dans /opt/ki-chat-src
   cd /opt/ki-chat-src
   cargo build --release -p ki-server
   mkdir -p /opt/ki-chat && cp target/release/ki-server /opt/ki-chat/
   useradd -r kichat && chown -R kichat /opt/ki-chat
   ```

3. Installe l'unité systemd fournie (`ki-server.service`) — pense à changer
   `KI_TOKEN` — puis :

   ```bash
   systemctl daemon-reload && systemctl enable --now ki-server
   ```

## Route B — Docker

Plus rien à compiler : l'image est publiée par GitHub Actions à chaque
poussée sur `main`, en amd64 et arm64.

```bash
docker run -d --name ki-chat --restart unless-stopped \
  -e KI_TOKEN=ton_code_secret \
  -p 9987:9987/udp -p 8080:8080/tcp \
  -v ki-chat-data:/data \
  ghcr.io/redik123/ki-chat-server:latest
```

Sur Jelastic : crée un nœud Docker depuis cette image, attache l'IP publique,
mappe les deux ports — l'UDP reste la condition, comme au-dessus.

Le guide complet (stack Portainer, mises à jour automatiques depuis GitHub,
sauvegarde du volume) est dans [`DEPLOY-DOCKER.md`](DEPLOY-DOCKER.md).

## TLS

**Rien à faire.** Le chat et la voix passent tous deux par QUIC sur le port
9987/udp, dans un tunnel TLS 1.3 dont le certificat auto-signé est généré au
premier démarrage et conservé dans `data/`. Pas d'autorité de certification à
solliciter, pas de reverse proxy à maintenir, pas de renouvellement.

Les clients se connectent avec l'adresse du serveur (`hôte` ou `hôte:port`).
Ils ne vérifient pas la chaîne du certificat — c'est un serveur privé — mais
le transport reste chiffré contre l'écoute passive, et la voix porte en plus
son propre chiffrement de bout en bout, que le serveur ne peut pas défaire.

Seul le **port HTTP 8080** reste en clair : il ne sert qu'à télécharger les
fichiers partagés, pour que les liens s'ouvrent dans un navigateur. Si tu
veux des liens en `https://` avec un vrai domaine, un reverse proxy devant ce
port suffit — sans effet sur le chat ni sur la voix.

## Côté clients (tes potes)

1. Récupèrent `ki-chat.exe` (build `cargo build --release -p ki-client-gui`,
   binaire dans `target/release/`).
2. Serveur : `IP_PUBLIQUE` tout court (ou `IP:9987` si port personnalisé).
3. Pseudo + mot de passe + **code d'invitation** (le `KI_TOKEN`, ou mieux :
   un code à usage unique généré dans ton panneau ♛ Admin).

## Checklist finale

- [ ] IP publique attachée, UDP 9987 ouvert (teste le vocal, pas juste le chat)
- [ ] `KI_TOKEN` fort (c'est le code de création de comptes)
- [ ] `data/` persistant (comptes, historique, fichiers partagés)
- [ ] Premier compte créé = **toi** (c'est lui qui devient admin)
- [ ] Sauvegarde régulière de `data/users.json`
