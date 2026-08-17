# syntax=docker/dockerfile:1
#
# Image du serveur ki-chat. Le serveur n'a aucune dépendance native — pas
# d'audio (il ne décode rien, il relaie), pas d'OpenSSL (rustls/ring) : la
# compilation croisée tient donc en un compilateur C par architecture, et
# l'image finale n'embarque qu'un binaire statique-de-fait sur la glibc.

FROM --platform=$BUILDPLATFORM rust:1-slim-bookworm AS build
ARG TARGETPLATFORM
WORKDIR /src

# On compile *pour* l'architecture visée depuis la machine de build, plutôt
# que d'émuler un runner ARM en QEMU : quelques minutes contre une heure.
RUN set -eux; \
    case "$TARGETPLATFORM" in \
      linux/amd64) target=x86_64-unknown-linux-gnu ;; \
      linux/arm64) target=aarch64-unknown-linux-gnu ;; \
      *) echo "plateforme non prise en charge : $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    echo "$target" > /target; \
    rustup target add "$target"; \
    if [ "$target" = aarch64-unknown-linux-gnu ] && [ "$(uname -m)" != aarch64 ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross; \
      rm -rf /var/lib/apt/lists/*; \
    fi
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc

COPY . .

# `--locked` : l'image se construit avec le Cargo.lock du dépôt, sinon deux
# builds du même tag pourraient embarquer deux jeux de dépendances.
# Le binaire est sorti du cache dans le même RUN — un montage de cache
# n'existe plus à la couche suivante.
# Les caches portent l'architecture dans leur identifiant : sans ça, les deux
# variantes d'une image multi-plateforme se disputeraient le même dossier et
# se compileraient l'une après l'autre.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=ki-cargo-registry-$TARGETPLATFORM,sharing=locked \
    --mount=type=cache,target=/src/target,id=ki-cargo-target-$TARGETPLATFORM,sharing=locked \
    set -eux; \
    target="$(cat /target)"; \
    cargo build --release --locked -p ki-server --target "$target"; \
    cp "target/$target/release/ki-server" /ki-server

FROM debian:bookworm-slim
LABEL org.opencontainers.image.source=https://github.com/Redik123/ki-chat \
      org.opencontainers.image.description="Serveur ki-chat : chat + relais vocal sur QUIC" \
      org.opencontainers.image.licenses=MIT

# curl ne sert qu'au HEALTHCHECK ; useradd, à ne pas tourner en root.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends curl; \
    rm -rf /var/lib/apt/lists/*; \
    useradd --system --uid 10001 --shell /usr/sbin/nologin kichat; \
    mkdir -p /data; \
    chown kichat:kichat /data

COPY --from=build /ki-server /usr/local/bin/ki-server

ENV KI_HTTP_PORT=8080 \
    KI_UDP_PORT=9987 \
    KI_DATA_DIR=/data
# KI_TOKEN (code d'invitation) : au lancement, jamais en dur dans l'image.

# Comptes, historique, fichiers partagés, identité du serveur et clé privée
# TLS. Perdre ce volume, c'est perdre les comptes et changer d'identité aux
# yeux des clients.
VOLUME /data
USER kichat
EXPOSE 8080/tcp 9987/udp

# Sonde volontairement minimale : le port des fichiers répond. Le transport
# QUIC, lui, se surveille tout seul — le serveur s'arrête si son écoute meurt,
# et la politique de redémarrage du conteneur fait le reste.
#
# `-k` : le certificat est auto-signé, et la sonde parle au serveur sur sa
# boucle locale. Ce qu'on vérifie ici est qu'il répond, pas à qui l'on parle.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD curl -fsSk "https://127.0.0.1:${KI_HTTP_PORT}/" || exit 1

ENTRYPOINT ["ki-server"]
