# Build du serveur ki-chat (le serveur n'a aucune dépendance audio :
# pas de libopus, pas de cmake — juste Rust).
FROM rust:1.97-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p ki-server

FROM debian:bookworm-slim
COPY --from=build /src/target/release/ki-server /usr/local/bin/ki-server
ENV KI_HTTP_PORT=8080 \
    KI_UDP_PORT=9987 \
    KI_DATA_DIR=/data
# KI_TOKEN (code d'invitation) : à définir au lancement, jamais en dur ici.
VOLUME /data
EXPOSE 8080/tcp 9987/udp
CMD ["ki-server"]
