# Changelog

Notable changes to mediabot. Versions follow the git tags: pushing a `vX.Y.Z` tag (matching `version` in
`Cargo.toml`) publishes the `:X.Y.Z` images and a GitHub release whose notes are that version's section below,
so add the section before tagging (see [Building and releasing](./README.md#building-and-releasing)).

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).


## [Unreleased]


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


[Unreleased]: https://github.com/cbodden/mediabot/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/cbodden/mediabot/releases/tag/v0.1.0
