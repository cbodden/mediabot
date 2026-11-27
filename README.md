# mediabot

An IRC bot (Rust) for Libera.Chat that reports on your media stack:

- **Media servers** — Plex, Emby, Jellyfin, Tautulli: now playing, transcodes, bandwidth, library counts
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
| `np [server]` | ro | now playing across Plex/Emby/Jellyfin/Tautulli |
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

## Deploy

- Docker: `Dockerfile` + `deploy/docker-compose.yml`. Use `listen = "0.0.0.0:8088"` inside the
  container and publish to `127.0.0.1` only.
- systemd: `deploy/mediabot.service`.

## Development

```sh
cargo test                 # unit tests: IRC parsing/splitting, ACL rules, config, encoding
cargo clippy --all-targets
RUST_LOG=debug cargo run   # shows raw IRC traffic (SASL payload redacted)
```

API notes: Plex uses `X-Plex-Token`; Jellyfin uses the `MediaBrowser Token=` authorization header;
Emby uses `X-Emby-Token`; Tautulli uses `apikey` (scrubbed from error messages); Seerr and the
\*arrs use `X-Api-Key`. Seerr searches are strictly percent-encoded (`%20`), as Seerr expects.
