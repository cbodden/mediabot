# syntax=mirror.gcr.io/docker/dockerfile:1
## two alpine stages: a static musl build, then plain alpine + the binary (~20 MB)
## bases come from mirror.gcr.io (google's mirror of docker hub) so builds never touch docker hub's pull limit
ARG ALPINE_VERSION=3.23

FROM mirror.gcr.io/library/rust:1-alpine${ALPINE_VERSION} AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
## dependencies first, in their own layer, so a source-only change doesn't rebuild them
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
 && cargo build --release --locked \
 && rm -rf src target/release/mediabot target/release/deps/mediabot-* target/release/.fingerprint/mediabot-*
COPY src src
COPY web web
COPY config.example.toml ./
RUN cargo build --release --locked && cp target/release/mediabot /mediabot

FROM mirror.gcr.io/library/alpine:${ALPINE_VERSION}
LABEL org.opencontainers.image.source="https://github.com/cbodden/mediabot" \
      org.opencontainers.image.description="IRC bot for Plex/Emby/Jellyfin/Tofa/Tautulli, Seerr and Sonarr/Radarr/Lidarr with a web panel and Prometheus/Loki telemetry" \
      org.opencontainers.image.licenses="MIT"
## ca-certificates: system CA roots for HTTPS backends (add your own CA under /usr/local/share/ca-certificates)
RUN apk add --no-cache ca-certificates \
 && adduser -D -H -u 10001 mediabot \
 && mkdir -p /data /config \
 && chown mediabot:mediabot /data
COPY --from=build /mediabot /usr/local/bin/mediabot
COPY config.example.toml /config/config.example.toml
## fail the build if the binary doesn't run on this base
RUN mediabot --version
USER mediabot
WORKDIR /data
## the config file comes from a mount; state (users, channels) lives in /data
ENV MEDIABOT_CONFIG=/config/config.toml \
    MEDIABOT_DATA_DIR=/data \
    MEDIABOT_WEB_LISTEN=0.0.0.0:8088
VOLUME /data
EXPOSE 8088
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
  CMD wget -q -O /dev/null "http://127.0.0.1:${MEDIABOT_WEB_LISTEN##*:}/healthz" || exit 1
ENTRYPOINT ["mediabot"]
