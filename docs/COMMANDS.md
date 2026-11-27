# Commands

<!-- generated from src/commands.rs - edit the COMMANDS table there, then run
     UPDATE_DOCS=1 cargo test commands_doc   (CI fails if this file is out of date) -->

Every command the bot understands (examples use the default nick `mediabot`). The same list is in the web panel's **Commands** card, and `!help <command>` shows any of them in IRC.

## Talking to the bot

- In a channel, start with the prefix (default `!`): `!np`. Or address it by its nick: `mediabot: np`.
- In a private message the prefix is optional: `/msg mediabot queue`.
- `!<service> [command]` targets one service by its `name` or `kind` from the config: `!plex`, `!sonarr upcoming 14`. With no command it means `np` for media servers, `requests` for Seerr and `queue` for Sonarr / Radarr / Lidarr.
- Answers longer than `max_lines` (default 8) are cut in channels; private messages get three times as many.
- People below admin can run one command every `cooldown_ms` (default 2 s).

## Roles

| Role | Can use |
|---|---|
| anyone | `whoami` only — everyone else without a role is ignored |
| readonly | every read command |
| admin | also `request`, `approve`, `decline` and managing read-only users |
| owner | everything, including `join` / `part` and managing admins (owners are set in the config file) |

## Quick reference

**General**

| Command | Role | Does |
|---|---|---|
| [`help`](#cmd-help) | readonly | list commands or show help for one |
| [`whoami`](#cmd-whoami) | anyone | show your identity and role |
| [`status`](#cmd-status) | readonly | backend health and versions |

**Media servers**

| Command | Role | Does |
|---|---|---|
| [`np`](#cmd-np) | readonly | what's playing on plex/emby/jellyfin/tofa/tautulli |
| [`libs`](#cmd-libs) | readonly | library item counts |

**Requests**

| Command | Role | Does |
|---|---|---|
| [`search`](#cmd-search) | readonly | search Seerr (movies + TV) and show availability |
| [`requests`](#cmd-requests) | readonly | list Seerr requests (default: pending) |
| [`request`](#cmd-request) | admin | create a Seerr request (ids come from search) |
| [`approve`](#cmd-approve) | admin | approve a pending Seerr request |
| [`decline`](#cmd-decline) | admin | decline a pending Seerr request |

**Downloads**

| Command | Role | Does |
|---|---|---|
| [`queue`](#cmd-queue) | readonly | download queues |
| [`upcoming`](#cmd-upcoming) | readonly | upcoming releases (default 7 days) |
| [`missing`](#cmd-missing) | readonly | monitored items without files |
| [`lookup`](#cmd-lookup) | readonly | look up a series/movie/artist and whether it's in the library |
| [`health`](#cmd-health) | readonly | \*arr health check warnings |

**Admin**

| Command | Role | Does |
|---|---|---|
| [`users`](#cmd-users) | admin | manage who can use the bot |
| [`join`](#cmd-join) | owner | join a channel (persisted) |
| [`part`](#cmd-part) | owner | leave a channel (persisted) |

## General

<a id="cmd-help"></a>

### help

```
!help [command]
```

- **Role:** readonly
- **Aliases:** `commands`

Without an argument, lists the commands your role can use. With one, shows its usage, required role and aliases.

Examples:

```
!help
!help request
```

<a id="cmd-whoami"></a>

### whoami

```
!whoami
```

- **Role:** anyone

Shows your hostmask, the NickServ account the bot sees (or "not identified") and your role. The only command that also answers people with no role.

Examples:

```
!whoami
```

<a id="cmd-status"></a>

### status

```
!status
```

- **Role:** readonly
- **Aliases:** `services`

Bot version, uptime, server, nick and channels, then every service from the last poll: ✓ with its version, or ✗ with the error.

Examples:

```
!status
```

## Media servers

<a id="cmd-np"></a>

### np

```
!np [service]
```

- **Role:** readonly
- **Aliases:** `streams`, `playing`, `nowplaying`

For each media server: stream count, how many transcode, total bandwidth, then one line per stream with user, title, quality, transcode or direct, progress and device. Servers with nothing playing are listed together at the end.

Examples:

```
!np
!np plex
!plex
```

<a id="cmd-libs"></a>

### libs

```
!libs [service]
```

- **Role:** readonly
- **Aliases:** `libraries`, `library`

Items per library for each media server, with episode or track totals for shows and music where the server reports them. An unknown count shows as ?.

Examples:

```
!libs
!libs jellyfin
```

## Requests

<a id="cmd-search"></a>

### search

```
!search <title>
```

- **Role:** readonly
- **Aliases:** `find`

Up to 5 movies and shows, each with its TMDB id and status (available, partially available, processing, pending, not requested). Use the id with request.

Examples:

```
!search the matrix
!search severance
```

<a id="cmd-requests"></a>

### requests

```
!requests [pending|approved|processing|available|unavailable|failed|completed|deleted|all]
```

- **Role:** readonly
- **Aliases:** `reqs`

The total for the filter, then up to 6 requests with id, title, type, who requested it, date and status. The id is what approve and decline take.

Examples:

```
!requests
!requests approved
!requests all
```

<a id="cmd-request"></a>

### request

```
!request <movie|tv> <tmdb-id> [seasons e.g. 1,2]
```

- **Role:** admin
- **Aliases:** `req`

Requests a movie or show by TMDB id. TV gets every season unless you list them. The request is made as the Seerr user who owns the API key, and logged to the audit trail with your name.

Examples:

```
!request movie 603
!request tv 95396
!request tv 95396 1,2
```

<a id="cmd-approve"></a>

### approve

```
!approve <request-id>
```

- **Role:** admin

Approves the request with that id (from requests). Logged to the audit trail.

Examples:

```
!approve 5
```

<a id="cmd-decline"></a>

### decline

```
!decline <request-id>
```

- **Role:** admin
- **Aliases:** `deny`

Declines the request with that id (from requests). Logged to the audit trail.

Examples:

```
!decline 6
```

## Downloads

<a id="cmd-queue"></a>

### queue

```
!queue [service]
```

- **Role:** readonly
- **Aliases:** `q`, `downloads`

For each Sonarr, Radarr and Lidarr (or the one named): queue size, then up to 5 items with status, progress and time left. Empty queues are listed together at the end.

Examples:

```
!queue
!queue sonarr
!sonarr
```

<a id="cmd-upcoming"></a>

### upcoming

```
!upcoming [service] [days]
```

- **Role:** readonly
- **Aliases:** `calendar`, `cal`

Episodes, movie releases (digital, physical, cinema) and albums in the next 7 days, or 1 to 60 days if you give a number. Up to 6 per service; ✓ marks episodes already downloaded.

Examples:

```
!upcoming
!upcoming 14
!sonarr upcoming 3
```

<a id="cmd-missing"></a>

### missing

```
!missing [service]
```

- **Role:** readonly
- **Aliases:** `wanted`

How many monitored items have no file yet, then up to 5 of them (episodes with air date, movies, albums).

Examples:

```
!missing
!missing radarr
```

<a id="cmd-lookup"></a>

### lookup

```
!lookup <service> <term>
```

- **Role:** readonly
- **Aliases:** `info`

Searches the service's metadata source and shows up to 4 matches, saying whether each is in your library and how complete (episodes or tracks on disk, movie downloaded or not).

Examples:

```
!lookup sonarr severance
!lookup radarr dune
!lookup lidarr blur
```

<a id="cmd-health"></a>

### health

```
!health [service]
```

- **Role:** readonly

The warnings and errors from each Sonarr, Radarr and Lidarr's own health checks (indexers, download clients, disk space…). Healthy services are listed together.

Examples:

```
!health
```

## Admin

<a id="cmd-users"></a>

### users

```
!users [list | add <account|mask> <readonly|admin> | del <account|mask>]
```

- **Role:** admin
- **Aliases:** `user`, `acl`

With no argument, lists owners, admins and read-only users. Entries are NickServ accounts, or nick!user@host globs for people without one. Owners can add and remove admins; admins can only add and remove read-only users. Owners themselves are set in the config file.

Examples:

```
!users
!users add alice readonly
!users add bob admin
!users del alice
```

<a id="cmd-join"></a>

### join

```
!join <#channel> [none|readonly] [key]
```

- **Role:** owner

Joins the channel and remembers it across restarts. The role is what people with no user entry get there (default none, meaning they're ignored).

Examples:

```
!join #movies
!join #family readonly
```

<a id="cmd-part"></a>

### part

```
!part <#channel>
```

- **Role:** owner
- **Aliases:** `leave`

Leaves the channel and forgets it, so it isn't rejoined on restart.

Examples:

```
!part #movies
```
