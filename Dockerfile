# syntax=docker/dockerfile:1
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY web web
COPY config.example.toml ./
RUN cargo build --release --locked && cp target/release/mediabot /mediabot

FROM alpine:3
RUN apk add --no-cache ca-certificates tini && adduser -D -u 10001 mediabot \
 && mkdir -p /data /config && chown mediabot /data
COPY --from=build /mediabot /usr/local/bin/mediabot
COPY config.example.toml /config/config.example.toml
USER mediabot
WORKDIR /data
ENV MEDIABOT_CONFIG=/config/config.toml
EXPOSE 8088
HEALTHCHECK --interval=30s --timeout=5s CMD wget -qO- http://127.0.0.1:8088/healthz || exit 1
ENTRYPOINT ["/sbin/tini", "--", "mediabot"]
