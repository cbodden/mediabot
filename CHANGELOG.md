# Changelog

Notable changes to mediabot. Versions follow the git tags: pushing a `vX.Y.Z` tag (matching `version` in
`Cargo.toml`) publishes the `:X.Y.Z` images and a GitHub release whose notes are that version's section below,
so add the section before tagging (see [Building and releasing](./README.md#building-and-releasing)).

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).


## [Unreleased]

### Fixed
- Seerr: `request`, `approve` and `decline` failed with `HTTP 403 {"message":"invalid csrf token"}` when Seerr's
  **CSRF Protection** is on (Settings → Network), because Seerr checks writes even when they use the API key.
  The bot now fetches Seerr's CSRF cookies, retries the write once with them and the `X-XSRF-TOKEN` header,
  and reuses them for later writes; with CSRF Protection off nothing changes (#6)

### Added
- `docs/COMMANDS.md`: every command with usage, required role, aliases, limits and examples. It is generated
  from the command table in `src/commands.rs` (the same table `!help` uses); a test fails CI if it's stale
  (`UPDATE_DOCS=1 cargo test commands_doc` regenerates it), and `mediabot --commands` prints it
- Web panel: a **Commands** card listing every command by group, with search and a role filter; clicking an
  example loads it into the console. Backed by the new `GET /api/commands`
- `!help <command>` also shows examples

### Changed
- README's command section now summarises and links to `docs/COMMANDS.md`


## [0.1.1] - 2026-11-27

### Fixed
- Docker images: the 0.1.0 images (`:0.1.0`, `:latest`, `:test`) contained an empty placeholder program instead
  of the bot, so the container exited immediately with no output and restarted in a loop. The image now builds
  once from the real sources, and the build fails unless the binary reports exactly the version in
  `Cargo.toml`; the final image checks it again
- README / Dockerfile: the image is ~8.6 MB to download (~29 MB on disk); the earlier "~4.5 MB" was
  measured on the broken 0.1.0 image

### Changed
- README rewritten as a step-by-step guide: setup in five steps, commands grouped by task, and full
  reference tables for configuration (with defaults), environment variables, the web API, metrics, Docker
  and each service's API; troubleshooting table


## [0.1.0] - 2026-11-26

### Added
- IRC client for Libera.Chat: TLS, IRCv3 CAP negotiation, SASL PLAIN, `account-tag` identity, flood
  control, ping timeouts, reconnect with backoff, NickServ REGAIN, rejoin on kick
- Media servers: Plex, Emby, Jellyfin, Tofa and Tautulli — now playing (`np`) and library counts (`libs`)
- Seerr (Overseerr / Jellyseerr API): `search`, `requests`, and for admins `request`, `approve`, `decline`
- Sonarr, Radarr, Lidarr: `queue`, `upcoming`, `missing`, `lookup`, `health`; `!<service> <command>`
  shorthand and several instances of one kind (e.g. `sonarr` + `sonarr4k`)
- Ownership: owners from the config, admins and read-only users added at runtime (NickServ accounts or
  hostmask globs), per-channel default role, all persisted in `data/state.json`
- Web control panel (HTTP or HTTPS, basic auth, CSRF header guard): IRC status and reconnect, service
  health and tests, users, channels, send message, command console, live logs
- Prometheus `/metrics`, log shipping to Loki with an audit trail of admin actions, Grafana dashboard
- Docker images for amd64 + arm64 on Alpine (`ghcr.io/cbodden/mediabot`, `cbodden/mediabot`), sample
  `docker-compose.yml` and `.env.example`, systemd unit
- `MEDIABOT_WEB_LISTEN` / `MEDIABOT_DATA_DIR` environment overrides; HTTPS backends trust the system CA
  store as well as the built-in Mozilla roots


[Unreleased]: https://github.com/cbodden/mediabot/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/cbodden/mediabot/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cbodden/mediabot/releases/tag/v0.1.0
