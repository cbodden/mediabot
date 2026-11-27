# mediabot

An IRC bot (Rust) for Libera.Chat that reports on your media stack:

- **Media servers** — Plex, Emby, Jellyfin, [Tofa](https://tofa.tv), Tautulli (Plex stats): now playing, transcodes, bandwidth, library counts
- **Requests** — Seerr (Overseerr / Jellyseerr API): search, request, list, approve/decline
- **\*arr** — Sonarr, Radarr, Lidarr: queue, upcoming, missing, library lookup, health
- **Ownership** — owners (config) → admins → read-only users, keyed on NickServ accounts
- **Web panel** — local HTTP(S) control panel with a command console, users, channels, services, live logs
- **Telemetry** — Prometheus `/metrics`, logs + audit trail to Loki, a ready-made Grafana dashboard

## Quick start

```sh
cargo build --release
cp config.example.toml config.toml        # edit: nick, channels, owners, services
export MEDIABOT_SASL_PASSWORD=... MEDIABOT_WEB_PASSWORD=... SONARR_API_KEY=...  # etc.
./target/release/mediabot --config config.toml --check   # validate
./target/release/mediabot --config config.toml
```

Open <http://127.0.0.1:8088> (basic auth from `[web]`).

**On Libera:** register the bot's nick with NickServ first (`/msg NickServ REGISTER …`) and set
`sasl_password`. Libera requires SASL from many hosting ranges, and the bot uses SASL PLAIN.
If the nick is taken by a ghost, the bot takes it back with `NickServ REGAIN`.

Any config value can be `${ENV_VAR}`. A missing variable is a startup error, never an empty string.

## Ownership and roles

| Role | Who | Can |
|---|---|---|
| owner | `bot.owners` in the config (NickServ accounts) | everything, including `join`/`part`; grants/revokes admins |
| admin | added at runtime | `request`, `approve`, `decline`, manage read-only users |
| readonly | added at runtime, or a channel's `default_role` | all read commands |
| none | everyone else | ignored (only `whoami` answers) |

Identity comes from the IRCv3 `account-tag` capability, so it is tied to the **NickServ account,
not the nick**. Changing nick can't impersonate anyone. For people without an account you can add a
hostmask glob such as `*!*@user/alice` instead.

Users live in `data_dir/state.json`, along with channels joined at runtime. Manage them from IRC:

```
!users add alice readonly
!users add bob admin            (owners only)
!users del alice
!users list
```

or from the **Users** card in the web panel.

## Commands

Prefix defaults to `!`. You can also address the bot (`mediabot: np`), or PM it without a prefix.
Replies in channels are capped at `max_lines` (PMs get 3×).

| Command | Role | |
|---|---|---|
| `help [cmd]`, `whoami`, `status` | ro / none / ro | help, your identity, backend health + versions |
| `np [server]` | ro | now playing across Plex/Emby/Jellyfin/Tofa/Tautulli |
| `libs [server]` | ro | library counts |
| `search <title>` | ro | Seerr search with availability + TMDB ids |
| `requests [pending\|approved\|…]` | ro | Seerr request list |
| `request <movie\|tv> <tmdb-id> [1,2]` | admin | create a Seerr request (TV defaults to all seasons) |
| `approve <id>` / `decline <id>` | admin | moderate Seerr requests |
| `queue [svc]`, `upcoming [svc] [days]`, `missing [svc]`, `health [svc]` | ro | \*arr info |
| `lookup <svc> <term>` | ro | look up a series/movie/artist and whether it's in the library |
| `users …` | admin | see above |
| `join <#chan> [default-role] [key]`, `part <#chan>` | owner | persisted |

Shorthand: `!<service> [command]`, e.g. `!sonarr upcoming 14`, `!radarr4k queue`, `!plex`.
Service names come from `name` in the config (default: the kind), so you can run several of the
same kind, such as `sonarr` and `sonarr4k`.

## Web panel

`[web] listen` defaults to `127.0.0.1:8088`. Set `tls_cert`/`tls_key` for HTTPS, or put it behind
a reverse proxy. The panel shows IRC state (reconnect button), service health with per-service
**Test**, users, channels (join/part), send-a-message, a **command console** (runs any bot command
as owner), and live logs.

The API is JSON under `/api/*` (same basic auth). State-changing requests must send an
`X-Mediabot: 1` header; this is CSRF protection, since browsers re-send cached basic-auth
credentials to cross-site requests.

## Metrics, logs, Grafana

- `GET /metrics` (no auth unless `metrics_auth = true`). All series are prefixed `mediabot_`:
  - bot: `irc_connected`, `irc_reconnects_total`, `irc_lines_total`, `irc_channels`, `commands_total{command,result}`, `command_duration_seconds`, `acl_users{role}`
  - backends: `service_up`, `service_requests_total`, `service_request_duration_seconds`
  - media: `streams{service,decision}`, `stream_bandwidth_kbps`, `library_items{library,type}`
  - requests: `seerr_requests{status}` (the raw Seerr counts: pending, approved, processing, available, declined, plus movie/tv/total)
  - \*arr: `arr_queue`, `arr_items`, `arr_missing`, `arr_health_issues{type}`
- Fast poll every `poll_interval_secs` (sessions, queues, counts). Library and missing totals
  are polled every `slow_poll_every` polls, because Radarr's movie list can be large.
- `[telemetry.loki]` ships all logs with labels `app=mediabot`, `host`, `level`, plus your own
  labels. Admin actions (requests, approvals, ACL changes, joins, web actions) are logged with
  target `audit`.
- `deploy/prometheus-scrape.yml` is a scrape job; `deploy/grafana-dashboard.json` is a dashboard
  (Prometheus + Loki datasource pickers) to import.

## Docker

Multi-arch images (**amd64 + arm64**) on Alpine, about 20 MB, holding a static musl binary,
`ca-certificates` and nothing else. The container runs as uid 10001.

| registry | image |
|---|---|
| GitHub | `ghcr.io/cbodden/mediabot` |
| Docker Hub | `cbodden/mediabot` |

| tag | built from |
|---|---|
| `:latest` | `main` (also rebuilt every Monday for Alpine updates) |
| `:test` | `testing` |
| `:X.Y.Z` | release tag `vX.Y.Z` |

```sh
cp config.example.toml config.toml   # nick, channels, owners, [[service]] entries
cp .env.example .env                 # the secrets config.toml references as ${VAR}
docker compose up -d                 # sample docker-compose.yml in the repo root
```

In the container:
- `/config/config.toml` is the config. Mount it read-only.
- `/data` holds the state (`state.json`: users and channels). Use a named volume, or a bind mount
  owned by uid 10001.
- The panel listens on `0.0.0.0:8088`. Publish it on `127.0.0.1` unless it's behind TLS.
- `MEDIABOT_WEB_LISTEN` and `MEDIABOT_DATA_DIR` override `[web] listen` and `[bot] data_dir`. The
  image sets them, so the same `config.toml` works in and out of Docker.
- `HEALTHCHECK` polls `/healthz`. It assumes the panel is enabled and serving plain HTTP.
- To trust a private CA (for a backend or Loki), mount its PEM file into `/etc/ssl/certs/`.
- While the repo is private, `ghcr.io` pulls need `docker login ghcr.io` with a PAT that has
  `read:packages`.

Without Docker: `deploy/mediabot.service` is a systemd unit.

### Building and releasing

- **Images** — `.github/workflows/docker-images.yml` builds each architecture on a native GitHub runner. Bases come
  from `mirror.gcr.io`, so builds never touch Docker Hub. It pushes to GHCR, then copies the tags to Docker Hub
  (needs the `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` repo secrets; without them that step is skipped).
  Pushes to `testing` publish `:test`, merges to `main` publish `:latest`, and `vX.Y.Z` tags publish `:X.Y.Z`.
- **Checks** — `.github/workflows/ci.yml` runs `cargo fmt --check`, clippy (`-D warnings`) and the tests on every
  push and PR.
- **Releases** — `CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). To release:
  1. In the `testing` → `main` PR, bump `version` in `Cargo.toml` and rename `## [Unreleased]` to
     `## [X.Y.Z] - date`.
  2. After the merge, tag the merge commit: `git tag -a vX.Y.Z -m "vX.Y.Z: summary" origin/main && git push origin vX.Y.Z`.
  3. `.github/workflows/release.yml` creates the GitHub release from that CHANGELOG section. It fails if the section is
     missing or `Cargo.toml` doesn't match the tag.

## Development

```sh
cargo test                 # unit tests: IRC parsing/splitting, ACL rules, config, encoding
cargo clippy --all-targets
RUST_LOG=debug cargo run   # shows raw IRC traffic (SASL payload redacted)
```

API notes: Plex uses `X-Plex-Token`; Jellyfin uses the `MediaBrowser Token=` authorization header;
Emby uses `X-Emby-Token`; Tofa uses `Authorization: Bearer <admin API key>` against
`/api/v1/system/playback/sessions` and `/api/v1/libraries` (Tofa doesn't publish response
schemas yet, so fields are parsed defensively and an unknown library count shows as `?`); Tautulli uses `apikey` (scrubbed from error messages); Seerr and the
\*arrs use `X-Api-Key`. Seerr searches are strictly percent-encoded (`%20`), as Seerr expects.
