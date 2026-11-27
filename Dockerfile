# syntax=mirror.gcr.io/docker/dockerfile:1
## two alpine stages: a static musl build, then plain alpine + the binary (~8.6 MB compressed, ~29 MB on disk)
## bases come from mirror.gcr.io (google's mirror of docker hub) so builds never touch docker hub's pull limit
ARG ALPINE_VERSION=3.23

FROM mirror.gcr.io/library/rust:1-alpine${ALPINE_VERSION} AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY web web
COPY config.example.toml ./
## one clean build from the real sources. 0.1.0 pre-built the dependencies with a placeholder main.rs in an
## earlier layer; cargo then treated that placeholder as up to date and the images shipped an empty binary.
## only the crate download cache is a cache mount - never target/, where old artifacts can look newer than
## freshly copied sources and get reused the same way.
## the build fails unless the binary reports exactly the version in Cargo.toml.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked \
 && cp target/release/mediabot /mediabot \
 && sed -n 's/^version = "\(.*\)"$/mediabot \1/p' Cargo.toml | head -1 > /mediabot.version \
 && echo "built: [$(/mediabot --version)]  expected: [$(cat /mediabot.version)]" \
 && test "$(/mediabot --version)" = "$(cat /mediabot.version)" \
 && rm -rf target

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
COPY --from=build /mediabot.version /tmp/expected-version
COPY config.example.toml /config/config.example.toml
## fail the build unless the real bot runs on this base: exact version, and its usage lists --config
RUN test "$(mediabot --version)" = "$(cat /tmp/expected-version)" \
 && mediabot --help | grep -q -- '--config' \
 && rm /tmp/expected-version
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
