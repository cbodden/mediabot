# mediabot

[![ci](https://github.com/cbodden/mediabot/actions/workflows/ci.yml/badge.svg)](https://github.com/cbodden/mediabot/actions/workflows/ci.yml)
[![docker images](https://github.com/cbodden/mediabot/actions/workflows/docker-images.yml/badge.svg)](https://github.com/cbodden/mediabot/actions/workflows/docker-images.yml)
[![release](https://img.shields.io/github/v/release/cbodden/mediabot)](https://github.com/cbodden/mediabot/releases)
[![license](https://img.shields.io/github/license/cbodden/mediabot)](LICENSE)

An IRC bot for [Libera.Chat](https://libera.chat), written in Rust, that answers questions about your
self-hosted media stack and lets trusted people act on it.

```
<you>      !np
<mediabot> plex: 2 stream(s), 1 transcoding, 12.0 Mbps
<mediabot>   ▶ alice — Severance S01E01 - Pilot [1080p, transcode] 50% on Living Room
<mediabot>   ⏸ bob — Dune (2021) [4k, direct] 10% on Plex Web
<you>      !search the matrix
<mediabot> [movie 603] The Matrix (1999) — available
<you>      !sonarr upcoming
<mediabot> sonarr: next 7 days
<mediabot>   2026-11-28 02:00Z  Severance S02E04 - Woe's Hollow
```

| Area | Services | What you get |
|---|---|---|
| Media servers | Plex, Emby, Jellyfin, [Tofa](https://tofa.tv), Tautulli — or all of them at once through [Tracearr](https://github.com/connorgallopo/Tracearr) | who's watching what, transcodes, bandwidth, library counts |
| Stats | Tracearr | plays and watch time today, most-watched movies and shows, recently added |
| Requests | Seerr (also Overseerr, Jellyseerr) | search, list, request, approve / decline |
| Downloads | Sonarr, Radarr, Lidarr | queue, upcoming releases, missing items, library lookup, health |
| Access | — | owners → admins → read-only users, tied to NickServ accounts |
| Control | — | a local web panel (HTTP or HTTPS) with a command console |
| Monitoring | Prometheus, Loki, Grafana | `/metrics`, log shipping with an audit trail, a ready-made dashboard |

Images for **amd64 and arm64** (Alpine, ~8.6 MB download): `cbodden/mediabot` on Docker Hub and
`ghcr.io/cbodden/mediabot`.

## Contents

1. [Set up](#set-up) — five steps from nothing to a working bot
2. [Using the bot](#using-the-bot) — roles, users and every command
3. [Web panel](#web-panel)
4. [Monitoring](#monitoring) — Prometheus, Grafana, Loki
5. [Configuration reference](#configuration-reference)
6. [Docker reference](#docker-reference)
7. [Supported services](#supported-services)
8. [Troubleshooting](#troubleshooting)
9. [Development and releases](#development-and-releases)

---

## Set up

### 1. Give the bot a Libera account

The bot logs in with SASL, which Libera requires from many hosting and VPN address ranges.
From any IRC client:

```
/nick mediabot
/msg NickServ REGISTER <password> <email>
/msg NickServ VERIFY REGISTER mediabot <code-from-the-email>
```

Also find **your own** account name, since owners are set by account, not by nick:
`/msg NickServ INFO <your-nick>` (or `/whois <your-nick>` → "is logged in as …").

Ask the channel's operators before bringing a bot in.

### 2. Collect the API keys

You only need the services you run.

| Service | `kind` | Default URL | Where to find the key |
|---|---|---|---|
| Plex | `plex` | `http://host:32400` | your `X-Plex-Token` ([how to find it](https://support.plex.tv/articles/204059436-finding-an-authentication-token-x-plex-token/)) |
| Jellyfin | `jellyfin` | `http://host:8096` | Dashboard → API Keys → **+** |
| Emby | `emby` | `http://host:8096` | Settings → Advanced → API Keys → **New API Key** |
| Tofa | `tofa` | `http://host:33333` | Admin → Settings → API → API keys ("Read only" is enough) |
| Tracearr | `tracearr` | `http://host:3000` | Settings → API (a `trr_pub_…` key) |
| Tautulli | `tautulli` | `http://host:8181` | Settings → Web Interface → API (enable, then copy) |
| Seerr / Overseerr / Jellyseerr | `seerr` | `http://host:5055` | Settings → General → API Key |
| Sonarr / Radarr / Lidarr | `sonarr` / `radarr` / `lidarr` | `:8989` / `:7878` / `:8686` | Settings → General → Security → API Key |

### 3. Write `config.toml`

Copy the fully commented [`config.example.toml`](config.example.toml) and edit it. This is the smallest useful
config:

```toml
[irc]
nick = "mediabot"
sasl_password = "${MEDIABOT_SASL_PASSWORD}"
channels = [{ name = "#my-media" }]

[bot]
owners = ["your-nickserv-account"]

[web]
password = "${MEDIABOT_WEB_PASSWORD}"

[[service]]
kind = "plex"
url = "http://plex:32400"
api_key = "${PLEX_TOKEN}"
```

`${NAME}` is replaced with the environment variable `NAME`, which keeps secrets out of the file. A variable
that isn't set stops the bot at startup instead of silently becoming an empty password.

### 4. Run it

**Docker (recommended)**, using the sample [`docker-compose.yml`](docker-compose.yml):

```sh
mkdir mediabot && cd mediabot
curl -LO https://raw.githubusercontent.com/cbodden/mediabot/main/docker-compose.yml
curl -LO https://raw.githubusercontent.com/cbodden/mediabot/main/.env.example
curl -L -o config.toml https://raw.githubusercontent.com/cbodden/mediabot/main/config.example.toml
cp .env.example .env        # fill in the passwords and keys
$EDITOR config.toml         # step 3
docker compose up -d
docker compose logs -f      # watch for "registered on irc.libera.chat as mediabot"
```

**From source** (Rust 1.88+):

```sh
cargo build --release
./target/release/mediabot --config config.toml --check   # validate only
./target/release/mediabot --config config.toml
```

**As a service:** [`deploy/mediabot.service`](deploy/mediabot.service) is a hardened systemd unit (secrets in
`/etc/mediabot/env`, state in `/var/lib/mediabot`).

### 5. Check it works

1. Open <http://127.0.0.1:8088> and log in with `[web] username` (default `admin`) and your password.
   The **IRC** card should say *connected*, and **Services** should show each backend as *up*.
2. In the channel, type `!whoami`. It should report your account and `role: owner`.
3. Type `!status` for every backend's version, then `!help`.

If something's wrong, see [Troubleshooting](#troubleshooting).

---

## Using the bot

### Talking to it

- In a channel, start with the prefix (default `!`): `!np`. Or address it by name: `mediabot: np`.
- In a private message, the prefix is optional: `/msg mediabot queue`.
- Long answers are cut at `max_lines` lines (default 8) in channels, and three times that in a PM.
- `!<service> [command]` targets one service: `!plex`, `!sonarr upcoming 14`, `!radarr4k queue`.
  With no command it means `np` for media servers, `requests` for Seerr and `queue` for the \*arrs.

### Roles

| Role | How someone gets it | What they can do |
|---|---|---|
| **owner** | listed in `[bot] owners` (config only) | everything, including `join` / `part`, granting and removing admins |
| **admin** | `!users add <account> admin` (by an owner) | `request`, `approve`, `decline`, add and remove read-only users |
| **readonly** | `!users add <account> readonly`, or a channel's `default_role` | every read command |
| **none** | everyone else | ignored, except `whoami` |

- **Identity is the NickServ account**, delivered by the IRCv3 `account-tag` capability, so changing nick
  can't impersonate anyone.
- For someone without an account, add a hostmask glob instead, such as `*!*@user/alice`.
- Non-admins are rate-limited to one command per `cooldown_ms` (default 2 s).

### Managing users

```
!users                       list owners, admins and read-only users
!users add alice readonly    add or update (also: ro, read)
!users add bob admin         owners only
!users del alice
```

Users, and channels joined at runtime, are saved to `state.json` in `data_dir`. The web panel's **Users**
card does the same.

### Commands

The full reference — every command with usage, required role, aliases, limits and examples — is in
**[docs/COMMANDS.md](docs/COMMANDS.md)**. The same list is in the web panel's **Commands** card, and
`!help <command>` shows any of them in IRC.

| Area | Commands |
|---|---|
| General | `help`, `whoami`, `status` |
| Media servers (Tracearr, or Plex, Emby, Jellyfin, Tofa, Tautulli) | `np`, `libs` |
| Stats (Tracearr) | `today`, `top`, `recent` |
| Requests (Seerr) | `search`, `requests`, and for admins `request`, `approve`, `decline` |
| Downloads (Sonarr, Radarr, Lidarr) | `queue`, `upcoming`, `missing`, `lookup`, `health` |
| Admin | `users` (admins), `join`, `part` (owners) |

---

## Web panel

Served on `[web] listen` (default `127.0.0.1:8088`), behind HTTP basic auth. It has cards for:

- **IRC**: server, nick, account, channels, uptime, last error, and a **Reconnect** button
- **Services**: up or down, version, a summary, last poll and latency, plus a **Test** button
- **Users** and **Channels**: add, remove, join and part
- **Commands**: every command with usage, role and examples, with a filter; clicking an example loads it
  into the console
- **Console**: runs any bot command as an owner, without IRC
- **Send message**: to a channel or nick
- **Logs**: live, info level and above

For HTTPS, set `tls_cert` and `tls_key` (PEM files), or put it behind a reverse proxy. Keep it off the public
internet without TLS.

### JSON API

Same basic auth. Every request other than GET must also send the header `X-Mediabot: 1`. That is CSRF
protection: browsers re-send cached basic-auth credentials on cross-site requests, but they can't add custom
headers to them.

| Method and path | Purpose |
|---|---|
| `GET /healthz` | liveness, no auth |
| `GET /metrics` | Prometheus, no auth unless `metrics_auth = true` |
| `GET /api/status` | version, start time, IRC state, services up / total |
| `GET /api/services` | `{media_source, services: [...]}`: per-service status, including `standby`, Tracearr's `backends`, and `enabled = false` services |
| `POST /api/services/{name}/test` | check a service now, returns version and latency |
| `GET /api/users` · `POST /api/users` `{"id","role"}` · `DELETE /api/users/{id}` | access list |
| `GET /api/channels` · `POST /api/channels` `{"name","key"?,"default_role"}` · `POST /api/channels/part` `{"name"}` | channels |
| `POST /api/say` `{"target","text"}` | send a message |
| `POST /api/command` `{"command"}` | run a bot command, returns `{"lines":[…]}` |
| `POST /api/reconnect` | reconnect to IRC |
| `GET /api/logs?since=<seq>` | log lines newer than `seq` (the last 1000 are kept) |
| `GET /api/commands` | the command reference: prefix, groups and every command |

```sh
curl -u admin:$PW -H 'X-Mediabot: 1' -H 'Content-Type: application/json' \
     -d '{"command":"queue"}' http://127.0.0.1:8088/api/command
```

---

## Monitoring

### Prometheus

The bot polls every service every `poll_interval_secs` (default 30). Library counts and missing totals are
slower to fetch, so they're polled every `slow_poll_every` polls (default 20, about 10 minutes). Media
servers on standby (see [Tracearr](#tracearr-and-media_source)) aren't polled, and their series are removed. Add
[`deploy/prometheus-scrape.yml`](deploy/prometheus-scrape.yml) to your scrape configs.

All series are prefixed `mediabot_`:

| Metric | Labels | Meaning |
|---|---|---|
| `irc_connected` | | 1 while registered on IRC |
| `irc_channels` | | channels joined |
| `irc_reconnects_total` | | connection attempts |
| `irc_lines_total` | `direction` | protocol lines in / out |
| `commands_total` | `command`, `result` | commands handled (`ok`, `error`, `denied`) |
| `command_duration_seconds` | `command` | histogram |
| `acl_users` | `role` | users per role |
| `service_up` | `service`, `kind` | 1 if the last poll worked |
| `service_requests_total` | `service`, `kind`, `result` | HTTP calls to backends |
| `service_request_duration_seconds` | `service`, `kind` | histogram |
| `streams` | `service`, `kind`, `decision` | active streams, `transcode` or `direct` |
| `stream_bandwidth_kbps` | `service`, `kind` | total streaming bandwidth |
| `library_items` | `service`, `kind`, `library`, `type` | items per library; `<type>_children` = episodes / tracks |
| `seerr_requests` | `service`, `status` | Seerr's request counts (`pending`, `approved`, `processing`, `available`, `declined`, `movie`, `tv`, `total`) |
| `arr_queue` · `arr_items` · `arr_missing` | `service`, `kind` | queue size, items managed, monitored-but-missing |
| `arr_health_issues` | `service`, `kind`, `type` | health check issues by level |
| `tracearr_server_up` | `service`, `server`, `type` | 1 if Tracearr reports that media server up |
| `tracearr_server_streams` | `service`, `server` | active streams per media server, from Tracearr |
| `tracearr_today` | `service`, `stat` | today's `plays`, `sessions`, `watch_hours`, `active_users`, `alerts` |
| `service_standby` | `service`, `kind` | 1 while Tracearr answers for that media server |
| `build_info` · `start_time_seconds` | `version` | build and start time |

### Grafana

Import [`deploy/grafana-dashboard.json`](deploy/grafana-dashboard.json) (Dashboards → New → Import). It asks
for a Prometheus and a Loki data source. It has 21 panels: overview stats, streams, bandwidth, queues,
missing, health, Seerr, libraries, backend latency and errors, commands, and the audit and warning logs.

### Loki

Add a `[telemetry.loki]` section and every log line is pushed with the labels `app="mediabot"`, `host`,
`level` and any you add. Requests, approvals, user changes, joins and web panel actions are logged with the
target **`audit`**, recording who did what.

```toml
[telemetry.loki]
url = "https://loki.example:3100"     # /loki/api/v1/push is added if missing
labels = { env = "home" }
# username / password for basic auth, tenant for X-Scope-OrgID
```

---

## Configuration reference

Every key below except `irc.nick` (and `web.password` while the panel is on) is optional.
[`config.example.toml`](config.example.toml) has comments for all of them.

### `[irc]`

| Key | Default | |
|---|---|---|
| `server` / `port` / `tls` | `irc.libera.chat` / `6697` / `true` | |
| `nick` | **required** | |
| `username` / `realname` | nick / `mediabot - media server stats` | |
| `sasl_account` / `sasl_password` | nick / none | no password means no SASL |
| `sasl_required` | `true` | disconnect (and retry) if SASL fails, rather than continue unidentified |
| `channels` | `[]` | list of `{ name, key?, default_role? }`; `default_role` is `none` (default) or `readonly` |
| `flood_burst` / `flood_interval_ms` | `4` / `1500` | outgoing rate limit: a burst, then one line per interval |
| `rejoin_on_kick` | `true` | rejoin after 10 s |

### `[bot]`

| Key | Default | |
|---|---|---|
| `prefix` | `"!"` | |
| `owners` | `[]` | NickServ account names |
| `default_role` | `"none"` | role for unknown users in private messages |
| `max_lines` | `8` | reply cap in channels (PMs get 3×) |
| `data_dir` | `"data"` | where `state.json` lives |
| `poll_interval_secs` | `30` | metrics poll interval (minimum 5) |
| `slow_poll_every` | `20` | library and missing totals every N polls |
| `cooldown_ms` | `2000` | per-user command interval for non-admins |
| `media_source` | `"auto"` | who answers `np` / `libs`: `auto`, `tracearr`, `direct` or `all` (see [Tracearr](#tracearr-and-media_source)) |
| `timezone` | `$TZ`, else UTC | IANA zone for Tracearr's "today" (`America/New_York`) |

### `[web]`

| Key | Default | |
|---|---|---|
| `enabled` | `true` | |
| `listen` | `127.0.0.1:8088` | |
| `username` / `password` | `admin` / **required if enabled** | |
| `tls_cert` / `tls_key` | none | PEM paths, set both for HTTPS |
| `metrics_auth` | `false` | require basic auth on `/metrics` too |

### `[telemetry]`

| Key | Default | |
|---|---|---|
| `log_level` | `info` | `RUST_LOG` overrides it; `debug` shows raw IRC traffic (SASL redacted) |
| `json_logs` | `false` | JSON lines on stdout |
| `[telemetry.loki]` | off | `url`, `labels`, `username`, `password`, `tenant` |

### `[[service]]` (repeat for each)

| Key | Default | |
|---|---|---|
| `enabled` | `true` | `false` switches the block off without deleting it; its `${VAR}`s may then be unset |
| `kind` | **required** | `plex`, `emby`, `jellyfin`, `tofa`, `tautulli`, `tracearr`, `seerr`, `sonarr`, `radarr`, `lidarr` |
| `url` / `api_key` | **required** | base URL (a sub-path like `/sonarr` is fine) |
| `name` | the kind | unique; used in commands and metric labels, so set it to run two of a kind (`sonarr4k`) |
| `verify_tls` | `true` | `false` for self-signed certificates |
| `timeout_secs` | `10` | per request |

### Environment variables

| Variable | |
|---|---|
| `MEDIABOT_CONFIG` | config path (default `config.toml`; also `--config`) |
| `MEDIABOT_WEB_LISTEN` | overrides `[web] listen` |
| `MEDIABOT_DATA_DIR` | overrides `[bot] data_dir` |
| `RUST_LOG` | overrides `log_level` |
| anything else | available to the config as `${NAME}` |

---

## Docker reference

| Registry | Image |
|---|---|
| Docker Hub | `cbodden/mediabot` |
| GitHub | `ghcr.io/cbodden/mediabot` |

| Tag | Built from |
|---|---|
| `latest` | `main`, and rebuilt every Monday to pick up Alpine fixes |
| `X.Y.Z` | the release tag `vX.Y.Z` |
| `test` | the `testing` branch, for trying changes before they reach `main` |

Inside the image:

| Path / setting | |
|---|---|
| `/config/config.toml` | your config, mounted read-only |
| `/data` | state (`state.json`); a named volume, or a bind mount owned by uid **10001** (`chown 10001:10001 data`) |
| port `8088` | web panel, `/metrics`, `/healthz`; publish it on `127.0.0.1` unless it's behind TLS |
| `MEDIABOT_WEB_LISTEN=0.0.0.0:8088`, `MEDIABOT_DATA_DIR=/data` | set by the image, so one `config.toml` works in and out of Docker |
| user | `mediabot` (uid 10001), not root |
| health check | `GET /healthz` every 30 s; needs the panel enabled and on plain HTTP |
| private CA | mount its PEM file into `/etc/ssl/certs/` for HTTPS backends or Loki |

---

## Supported services

| Service | Auth used | API calls |
|---|---|---|
| Plex | `X-Plex-Token` header | `/status/sessions`, `/library/sections`, `/identity` |
| Jellyfin | `Authorization: MediaBrowser Token="…"` | `/Sessions`, `/Items/Counts`, `/System/Info` |
| Emby | `X-Emby-Token` header | `/Sessions`, `/Items/Counts`, `/System/Info` |
| Tofa | `Authorization: Bearer <key>` | `/api/v1/system/playback/sessions`, `/api/v1/libraries`, `/api/v1/system/info` |
| Tracearr | `Authorization: Bearer trr_pub_…` | `/api/v2/public/streams`, `/libraries`, `/servers`, `/top-media`, `/recently-added`; v1 `/api/v1/public/stats/today` |
| Tautulli | `apikey` query parameter (removed from error messages) | `/api/v2?cmd=get_activity`, `get_libraries`, `get_tautulli_info` |
| Seerr | `X-Api-Key` header | `/api/v1/search`, `/request`, `/request/count`, `/request/{id}/approve\|decline`, `/movie/{id}`, `/tv/{id}` |
| Sonarr / Radarr | `X-Api-Key` header | `/api/v3/queue`, `/calendar`, `/wanted/missing` or `/movie`, `/{series\|movie}/lookup`, `/health`, `/system/status` |
| Lidarr | `X-Api-Key` header | the same under `/api/v1` with `/artist` |

Notes:

- **Tofa**'s API is in beta and doesn't publish response formats, so the bot reads several possible field
  names. A library count it can't find shows as `?`. If something looks wrong,
  [open an issue](https://github.com/cbodden/mediabot/issues) with the output of
  `/api/v1/libraries` or `/api/v1/system/playback/sessions`.
- **Seerr** with **CSRF Protection** on (Settings → Network) is supported: Seerr checks writes even when
  they use the API key, so for `request`, `approve` and `decline` the bot fetches Seerr's CSRF cookies and
  sends the token back. Reads don't need it.
- **Tautulli** reports on the same Plex server you may also list as `plex`, so `np` then shows those streams
  twice. Configure whichever you prefer.

### Tracearr and `media_source`

[Tracearr](https://github.com/connorgallopo/Tracearr) monitors Plex, Jellyfin, Emby and Tofa together. With a
`kind = "tracearr"` service, `np` and `libs` can come from it alone, and the stats commands (`today`, `top`,
`recent`) become available. You keep the direct server blocks; `[bot] media_source` decides who answers:

| `media_source` | `np` / `libs` with no service named | direct servers polled |
|---|---|---|
| `auto` (default) | Tracearr while it's up, each stream labelled with its server; the direct servers if it's down | only while Tracearr is down |
| `tracearr` | Tracearr only | never (standby) |
| `direct` | the direct servers only | always |
| `all` | everything (a stream can show twice) | always |

- A direct server Tracearr is answering for is on **standby**: not polled, shown as `⏸ standby` in `!status`
  and the panel, its stream / library / up series removed, and `mediabot_service_standby` = 1.
- If Tracearr fails in the middle of a command, the bot says so and asks the direct servers instead.
- Naming a service always asks that one: `!np plex`, `!plex`.
- To switch a server off completely, set `enabled = false` in its block.
- Tracearr's library API has counts but no library names, so `libs` totals them per server: movies,
  shows (episodes) and tracks.

---

## Troubleshooting

| Symptom | Likely cause and fix |
|---|---|
| Log says `SASL failed (904)` | wrong `sasl_password`, or `sasl_account` isn't the registered account |
| Connects but never joins | the channel needs `key`, is invite-only, or bans the bot (the log says `cannot join`) |
| The bot ignores you | `!whoami` — if it says `not identified`, log in to NickServ; if `role: none`, add yourself to `owners` or `!users add` |
| `permission denied: … needs admin` | your role is too low; an owner can raise it |
| A service shows ✗ `HTTP 401` / `403` | wrong `api_key`, or the URL points at the wrong service |
| A service shows ✗ `connect failed` | wrong `url`, or (in Docker) a hostname the container can't resolve |
| HTTPS backend: certificate error | `verify_tls = false`, or mount your CA into `/etc/ssl/certs/` |
| Panel unreachable in Docker | publish port 8088; don't override `MEDIABOT_WEB_LISTEN` to `127.0.0.1` |
| `Permission denied` on `/data` | a bind mount must be owned by uid 10001, or use a named volume |
| Startup error `config references unset env var X` | set `X` (for example in `.env`) |

Run `mediabot --check` to validate a config without connecting, and `mediabot --commands` to print the
command reference. Set `RUST_LOG=debug` to see raw IRC traffic.

---

## Development and releases

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
RUST_LOG=debug cargo run -- --config config.toml
```

- **Command docs**: `docs/COMMANDS.md` is generated from the `COMMANDS` table in `src/commands.rs`.
  After changing a command, run `UPDATE_DOCS=1 cargo test commands_doc`; CI fails while the file is stale.
- **Branches**: work lands on `testing` (published as `:test`), then reaches `main` (`:latest`) by pull request.
- **CI** ([`ci.yml`](.github/workflows/ci.yml)): format, clippy and tests on every push and pull request.
- **Images** ([`docker-images.yml`](.github/workflows/docker-images.yml)):
  - each architecture builds on a native runner; base images come from `mirror.gcr.io`, so Docker Hub's pull
    limit never affects a build
  - results go to GHCR, then are copied to Docker Hub (needs the `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN`
    secrets)
- **Releases** ([`release.yml`](.github/workflows/release.yml)), using [CHANGELOG.md](CHANGELOG.md):
  1. In the `testing` → `main` pull request, bump `version` in `Cargo.toml` and turn `## [Unreleased]` into
     `## [X.Y.Z] - date`.
  2. After the merge, tag the merge commit: `git tag -a vX.Y.Z -m "vX.Y.Z: summary" origin/main && git push origin vX.Y.Z`.
  3. That builds the `:X.Y.Z` images and publishes the GitHub release from the CHANGELOG section. It fails if
     the section is missing or the tag doesn't match `Cargo.toml`.

## License

[MIT](LICENSE)
