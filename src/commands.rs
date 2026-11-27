//! Chat commands. The same executor serves IRC and the web console.

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use tokio::{sync::mpsc, task::JoinSet};
use tracing::{info, warn};

use crate::{
    acl::{Role, Sender, irc_lower, is_mask},
    app::App,
    config::{ChannelConfig, ServiceKind},
    irc::{ChatMessage, IrcCmd},
    services::{Service, seerr},
};

const B: &str = "\x02"; // IRC bold

pub struct Ctx {
    pub sender: Sender,
    pub role: Role,
    /// Who to credit in audit logs, e.g. "account:alice" or "web:admin".
    pub actor: String,
}

/// One chat command. This table is the single source for `!help`, the web panel's command list
/// (`GET /api/commands`) and `docs/COMMANDS.md` (kept in sync by a test).
#[derive(Debug, serde::Serialize)]
pub struct Spec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// Minimum role needed.
    pub role: Role,
    /// Section heading in the docs and the panel.
    pub group: &'static str,
    pub usage: &'static str,
    /// One line, shown by `!help`.
    pub summary: &'static str,
    /// Longer explanation for the docs and the panel (limits, defaults, filters).
    pub details: &'static str,
    /// Typed without the prefix.
    pub examples: &'static [&'static str],
}

pub const GROUPS: &[&str] = &[
    "General",
    "Media servers",
    "Stats",
    "Requests",
    "Downloads",
    "Admin",
];

pub const COMMANDS: &[Spec] = &[
    Spec {
        name: "help",
        aliases: &["commands"],
        role: Role::ReadOnly,
        group: "General",
        usage: "help [command]",
        summary: "list commands or show help for one",
        details: "Without an argument, lists the commands your role can use. With one, shows its usage, \
                  required role and aliases.",
        examples: &["help", "help request"],
    },
    Spec {
        name: "whoami",
        aliases: &[],
        role: Role::None,
        group: "General",
        usage: "whoami",
        summary: "show your identity and role",
        details: "Shows your hostmask, the NickServ account the bot sees (or \"not identified\") and your \
                  role. The only command that also answers people with no role.",
        examples: &["whoami"],
    },
    Spec {
        name: "status",
        aliases: &["services"],
        role: Role::ReadOnly,
        group: "General",
        usage: "status",
        summary: "backend health and versions",
        details: "Bot version, uptime, server, nick and channels, then every service from the last poll: \
                  ✓ with its version, ✗ with the error, or ⏸ standby for a media server Tracearr is \
                  answering for. Tracearr also lists each media server it monitors as up or down. \
                  Services switched off with enabled = false are listed last.",
        examples: &["status"],
    },
    Spec {
        name: "np",
        aliases: &["streams", "playing", "nowplaying"],
        role: Role::ReadOnly,
        group: "Media servers",
        usage: "np [service]",
        summary: "what's playing (Tracearr, or plex/emby/jellyfin/tofa/tautulli)",
        details: "Stream count, how many transcode, total bandwidth, then one line per stream with user, \
                  title, quality, transcode or direct, progress and device. With Tracearr configured \
                  (media_source = auto) it answers for every server at once and each stream says which \
                  server it's on; if Tracearr is down the servers are asked directly. Naming a service \
                  always asks that one. Servers with nothing playing are listed together at the end.",
        examples: &["np", "np plex", "plex", "tracearr"],
    },
    Spec {
        name: "libs",
        aliases: &["libraries", "library"],
        role: Role::ReadOnly,
        group: "Media servers",
        usage: "libs [service]",
        summary: "library item counts",
        details: "Items per library for each media server, with episode or track totals for shows and \
                  music where the server reports them. An unknown count shows as ?. Through Tracearr, \
                  counts are totalled per server: movies, shows (episodes), tracks.",
        examples: &["libs", "libs jellyfin"],
    },
    Spec {
        name: "today",
        aliases: &[],
        role: Role::ReadOnly,
        group: "Stats",
        usage: "today",
        summary: "today's plays, watch time and active users (Tracearr)",
        details: "From Tracearr, across all its servers: plays and sessions so far today, hours watched, \
                  active users, streams playing now, and any sharing alerts in the last 24 hours. \
                  \"Today\" uses [bot] timezone, else $TZ, else UTC.",
        examples: &["today", "tracearr today"],
    },
    Spec {
        name: "top",
        aliases: &["popular"],
        role: Role::ReadOnly,
        group: "Stats",
        usage: "top [movies|shows] [days] [plays|time|viewers]",
        summary: "most-watched movies and shows (Tracearr)",
        details: "Without a type: the top 3 movies and top 3 shows; with one, the top 5. Over the last 7 \
                  days unless you give a number (1 to 365), ranked by plays unless you say time (hours \
                  watched) or viewers (different users). Each line shows plays, hours and viewers.",
        examples: &["top", "top shows 30", "top movies 7 viewers"],
    },
    Spec {
        name: "recent",
        aliases: &["new", "added"],
        role: Role::ReadOnly,
        group: "Stats",
        usage: "recent [movies|shows|episodes|music|artists|tracks] [count]",
        summary: "recently added to the libraries (Tracearr)",
        details: "The newest additions across every server Tracearr monitors, newest first: date and time \
                  (UTC), title, year, type and server. 6 by default, up to 15. Items since removed are \
                  left out.",
        examples: &["recent", "recent movies", "recent episodes 10"],
    },
    Spec {
        name: "search",
        aliases: &["find"],
        role: Role::ReadOnly,
        group: "Requests",
        usage: "search <title>",
        summary: "search Seerr (movies + TV) and show availability",
        details: "Up to 5 movies and shows, each with its TMDB id and status (available, partially \
                  available, processing, pending, not requested). Use the id with request.",
        examples: &["search the matrix", "search severance"],
    },
    Spec {
        name: "requests",
        aliases: &["reqs"],
        role: Role::ReadOnly,
        group: "Requests",
        usage: "requests [pending|approved|processing|available|unavailable|failed|completed|deleted|all]",
        summary: "list Seerr requests (default: pending)",
        details: "The total for the filter, then up to 6 requests with id, title, type, who requested it, \
                  date and status. The id is what approve and decline take.",
        examples: &["requests", "requests approved", "requests all"],
    },
    Spec {
        name: "request",
        aliases: &["req"],
        role: Role::Admin,
        group: "Requests",
        usage: "request <movie|tv> <tmdb-id> [seasons e.g. 1,2]",
        summary: "create a Seerr request (ids come from search)",
        details: "Requests a movie or show by TMDB id. TV gets every season unless you list them. The \
                  request is made as the Seerr user who owns the API key, and logged to the audit trail \
                  with your name.",
        examples: &[
            "request movie 603",
            "request tv 95396",
            "request tv 95396 1,2",
        ],
    },
    Spec {
        name: "approve",
        aliases: &[],
        role: Role::Admin,
        group: "Requests",
        usage: "approve <request-id>",
        summary: "approve a pending Seerr request",
        details: "Approves the request with that id (from requests). Logged to the audit trail.",
        examples: &["approve 5"],
    },
    Spec {
        name: "decline",
        aliases: &["deny"],
        role: Role::Admin,
        group: "Requests",
        usage: "decline <request-id>",
        summary: "decline a pending Seerr request",
        details: "Declines the request with that id (from requests). Logged to the audit trail.",
        examples: &["decline 6"],
    },
    Spec {
        name: "queue",
        aliases: &["q", "downloads"],
        role: Role::ReadOnly,
        group: "Downloads",
        usage: "queue [service]",
        summary: "download queues",
        details: "For each Sonarr, Radarr and Lidarr (or the one named): queue size, then up to 5 items with \
                  status, progress and time left. Empty queues are listed together at the end.",
        examples: &["queue", "queue sonarr", "sonarr"],
    },
    Spec {
        name: "upcoming",
        aliases: &["calendar", "cal"],
        role: Role::ReadOnly,
        group: "Downloads",
        usage: "upcoming [service] [days]",
        summary: "upcoming releases (default 7 days)",
        details: "Episodes, movie releases (digital, physical, cinema) and albums in the next 7 days, or 1 to \
                  60 days if you give a number. Up to 6 per service; ✓ marks episodes already downloaded.",
        examples: &["upcoming", "upcoming 14", "sonarr upcoming 3"],
    },
    Spec {
        name: "missing",
        aliases: &["wanted"],
        role: Role::ReadOnly,
        group: "Downloads",
        usage: "missing [service]",
        summary: "monitored items without files",
        details: "How many monitored items have no file yet, then up to 5 of them (episodes with air date, \
                  movies, albums).",
        examples: &["missing", "missing radarr"],
    },
    Spec {
        name: "lookup",
        aliases: &["info"],
        role: Role::ReadOnly,
        group: "Downloads",
        usage: "lookup <service> <term>",
        summary: "look up a series/movie/artist and whether it's in the library",
        details: "Searches the service's metadata source and shows up to 4 matches, saying whether each is \
                  in your library and how complete (episodes or tracks on disk, movie downloaded or not).",
        examples: &[
            "lookup sonarr severance",
            "lookup radarr dune",
            "lookup lidarr blur",
        ],
    },
    Spec {
        name: "health",
        aliases: &[],
        role: Role::ReadOnly,
        group: "Downloads",
        usage: "health [service]",
        summary: "*arr health check warnings",
        details: "The warnings and errors from each Sonarr, Radarr and Lidarr's own health checks (indexers, \
                  download clients, disk space…). Healthy services are listed together.",
        examples: &["health"],
    },
    Spec {
        name: "users",
        aliases: &["user", "acl"],
        role: Role::Admin,
        group: "Admin",
        usage: "users [list | add <account|mask> <readonly|admin> | del <account|mask>]",
        summary: "manage who can use the bot",
        details: "With no argument, lists owners, admins and read-only users. Entries are NickServ accounts, \
                  or nick!user@host globs for people without one. Owners can add and remove admins; admins \
                  can only add and remove read-only users. Owners themselves are set in the config file.",
        examples: &[
            "users",
            "users add alice readonly",
            "users add bob admin",
            "users del alice",
        ],
    },
    Spec {
        name: "join",
        aliases: &[],
        role: Role::Owner,
        group: "Admin",
        usage: "join <#channel> [none|readonly] [key]",
        summary: "join a channel (persisted)",
        details: "Joins the channel and remembers it across restarts. The role is what people with no user \
                  entry get there (default none, meaning they're ignored).",
        examples: &["join #movies", "join #family readonly"],
    },
    Spec {
        name: "part",
        aliases: &["leave"],
        role: Role::Owner,
        group: "Admin",
        usage: "part <#channel>",
        summary: "leave a channel (persisted)",
        details: "Leaves the channel and forgets it, so it isn't rejoined on restart.",
        examples: &["part #movies"],
    },
];

fn role_label(r: Role) -> &'static str {
    match r {
        Role::None => "anyone",
        r => r.as_str(),
    }
}

fn md_escape(s: &str) -> String {
    s.replace('*', "\\*")
}

/// `docs/COMMANDS.md`, rendered from [`COMMANDS`]. A test keeps the file in sync.
pub fn commands_markdown() -> String {
    use std::fmt::Write as _;
    let mut md = String::new();
    let _ = write!(
        md,
        "# Commands\n\n\
         <!-- generated from src/commands.rs - edit the COMMANDS table there, then run\n     \
         UPDATE_DOCS=1 cargo test commands_doc   (CI fails if this file is out of date) -->\n\n\
         Every command the bot understands (examples use the default nick `mediabot`). The same list is in the web panel's **Commands** card, \
         and `!help <command>` shows any of them in IRC.\n\n\
         ## Talking to the bot\n\n\
         - In a channel, start with the prefix (default `!`): `!np`. Or address it by its nick: `mediabot: np`.\n\
         - In a private message the prefix is optional: `/msg mediabot queue`.\n\
         - `!<service> [command]` targets one service by its `name` or `kind` from the config: `!plex`, \
         `!sonarr upcoming 14`. With no command it means `np` for media servers, `requests` for Seerr and \
         `queue` for Sonarr / Radarr / Lidarr.\n\
         - Answers longer than `max_lines` (default 8) are cut in channels; private messages get three times as many.\n\
         - People below admin can run one command every `cooldown_ms` (default 2 s).\n\n\
         ## Roles\n\n\
         | Role | Can use |\n|---|---|\n\
         | anyone | `whoami` only — everyone else without a role is ignored |\n\
         | readonly | every read command |\n\
         | admin | also `request`, `approve`, `decline` and managing read-only users |\n\
         | owner | everything, including `join` / `part` and managing admins (owners are set in the config file) |\n\n\
         ## Quick reference\n\n"
    );
    for g in GROUPS {
        let _ = write!(md, "**{g}**\n\n| Command | Role | Does |\n|---|---|---|\n");
        for c in COMMANDS.iter().filter(|c| c.group == *g) {
            let _ = writeln!(
                md,
                "| [`{}`](#cmd-{}) | {} | {} |",
                c.name,
                c.name,
                role_label(c.role),
                md_escape(c.summary)
            );
        }
        md.push('\n');
    }
    for g in GROUPS {
        let _ = write!(md, "## {g}\n\n");
        for c in COMMANDS.iter().filter(|c| c.group == *g) {
            let _ = write!(
                md,
                "<a id=\"cmd-{}\"></a>\n\n### {}\n\n```\n!{}\n```\n\n",
                c.name, c.name, c.usage
            );
            let _ = writeln!(md, "- **Role:** {}", role_label(c.role));
            if !c.aliases.is_empty() {
                let al: Vec<String> = c.aliases.iter().map(|a| format!("`{a}`")).collect();
                let _ = writeln!(md, "- **Aliases:** {}", al.join(", "));
            }
            let details = c.details.split_whitespace().collect::<Vec<_>>().join(" ");
            let _ = write!(md, "\n{}\n\n", md_escape(&details));
            if !c.examples.is_empty() {
                md.push_str("Examples:\n\n```\n");
                for e in c.examples {
                    let _ = writeln!(md, "!{e}");
                }
                md.push_str("```\n\n");
            }
        }
    }
    md.truncate(md.trim_end().len());
    md.push('\n');
    md
}

fn find(name: &str) -> Option<&'static Spec> {
    let n = name.to_ascii_lowercase();
    COMMANDS
        .iter()
        .find(|c| c.name == n || c.aliases.contains(&n.as_str()))
}

/// Consume chat messages from IRC.
pub async fn run(app: Arc<App>, mut rx: mpsc::UnboundedReceiver<ChatMessage>) {
    while let Some(msg) = rx.recv().await {
        let app = app.clone();
        tokio::spawn(async move { on_message(app, msg).await });
    }
}

async fn on_message(app: Arc<App>, msg: ChatMessage) {
    let prefix = &app.config.bot.prefix;
    let text = msg.text.trim();
    let nick = app.irc_status.read().await.nick.clone();
    let body = if let Some(b) = text.strip_prefix(prefix.as_str()) {
        b
    } else if let Some(b) = strip_addressed(text, &nick) {
        b
    } else if msg.channel.is_none() {
        text // PMs don't need the prefix
    } else {
        return;
    };
    let body = body.trim();
    if body.is_empty() {
        return;
    }

    let fallback = match &msg.channel {
        Some(ch) => app
            .store
            .channels(&app.config.irc.channels)
            .await
            .into_iter()
            .find(|c| irc_lower(&c.name) == irc_lower(ch))
            .map(|c| c.default_role)
            .unwrap_or(Role::None),
        None => app.config.bot.default_role,
    };
    let role = app.store.role_of(&msg.sender, fallback).await;
    let cmd_word = body.split_whitespace().next().unwrap_or("");
    // Unknown users are ignored entirely (except whoami) so the bot can't be used to flood.
    if role == Role::None && !cmd_word.eq_ignore_ascii_case("whoami") {
        return;
    }
    if role < Role::Admin && !cooldown_ok(&app, &msg.sender).await {
        return;
    }

    let ctx = Ctx {
        actor: match &msg.sender.account {
            Some(a) => format!("{} (account {a})", msg.sender.nick),
            None => msg.sender.mask(),
        },
        sender: msg.sender.clone(),
        role,
    };
    let lines = execute(&app, &ctx, body).await;
    let max = if msg.channel.is_some() {
        app.config.bot.max_lines
    } else {
        app.config.bot.max_lines * 3
    };
    let target = msg.reply_target().to_string();
    for l in truncate(lines, max) {
        app.irc.say(&target, l);
    }
}

fn strip_addressed<'a>(text: &'a str, nick: &str) -> Option<&'a str> {
    if nick.is_empty() || text.len() <= nick.len() || !text.is_char_boundary(nick.len()) {
        return None;
    }
    let (head, rest) = text.split_at(nick.len());
    (irc_lower(head) == irc_lower(nick) && rest.starts_with([':', ',']))
        .then(|| rest[1..].trim_start())
}

async fn cooldown_ok(app: &App, s: &Sender) -> bool {
    let key = s.account.clone().unwrap_or_else(|| s.host.clone());
    let mut cd = app.cooldowns.lock().await;
    let now = Instant::now();
    let window = Duration::from_millis(app.config.bot.cooldown_ms);
    cd.retain(|_, t| now.duration_since(*t) < window);
    if cd.contains_key(&key) {
        return false;
    }
    cd.insert(key, now);
    true
}

fn truncate(mut lines: Vec<String>, max: usize) -> Vec<String> {
    if lines.len() > max {
        let extra = lines.len() - (max - 1);
        lines.truncate(max - 1);
        lines.push(format!(
            "… and {extra} more (narrow it down, or ask me in a PM)"
        ));
    }
    lines
}

/// Run one command line and return the reply lines. Never panics; errors become a line.
pub async fn execute(app: &Arc<App>, ctx: &Ctx, line: &str) -> Vec<String> {
    let mut words: Vec<&str> = line.split_whitespace().collect();
    if words.is_empty() {
        return vec![];
    }
    // Shorthand: "!sonarr queue" == "!queue sonarr", "!plex" == "!np plex".
    if find(words[0]).is_none() && app.is_service_word(words[0]) {
        let svc = words.remove(0);
        let sub = if words.is_empty() {
            default_sub(app, svc)
        } else {
            words.remove(0)
        };
        let mut w = vec![sub, svc];
        w.extend(words);
        words = w;
    }
    let Some(spec) = find(words[0]) else {
        return vec![format!(
            "unknown command {:?} — try {}help",
            words[0], app.config.bot.prefix
        )];
    };
    let args = &words[1..];
    let started = Instant::now();
    let result = if ctx.role < spec.role {
        Err(anyhow!(
            "permission denied: {} needs {} (you are {})",
            spec.name,
            spec.role.as_str(),
            ctx.role.as_str()
        ))
    } else {
        dispatch(app, ctx, spec, args).await
    };
    app.metrics
        .command_duration
        .with_label_values(&[spec.name])
        .observe(started.elapsed().as_secs_f64());
    let label = match &result {
        Ok(_) => "ok",
        Err(e) if e.to_string().starts_with("permission denied") => "denied",
        Err(_) => "error",
    };
    app.metrics
        .commands
        .with_label_values(&[spec.name, label])
        .inc();
    match result {
        Ok(lines) => lines,
        Err(e) => {
            if label == "error" {
                warn!(command = spec.name, actor = %ctx.actor, "command failed: {e:#}");
            }
            vec![format!("{}: {e:#}", spec.name)]
        }
    }
}

fn default_sub<'a>(app: &App, svc: &str) -> &'a str {
    let kind = app
        .services
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(svc) || s.kind.as_str().eq_ignore_ascii_case(svc))
        .map(|s| s.kind);
    match kind {
        Some(k) if k.is_media_server() => "np",
        Some(ServiceKind::Seerr) => "requests",
        _ => "queue",
    }
}

async fn dispatch(app: &Arc<App>, ctx: &Ctx, spec: &Spec, args: &[&str]) -> Result<Vec<String>> {
    match spec.name {
        "help" => Ok(help(app, ctx, args.first().copied())),
        "whoami" => Ok(vec![format!(
            "{} — account: {} — role: {B}{}{B}",
            ctx.sender.mask(),
            ctx.sender.account.as_deref().unwrap_or("(not identified)"),
            ctx.role.as_str()
        )]),
        "status" => status(app).await,
        "np" => now_playing(app, args.first().copied()).await,
        "libs" => libraries(app, args.first().copied()).await,
        "today" => today(app, args).await,
        "top" => top(app, args).await,
        "recent" => recent(app, args).await,
        "search" => search(app, args).await,
        "requests" => requests(app, args.first().copied()).await,
        "request" => request(app, ctx, args).await,
        "approve" => moderate(app, ctx, args, "approve").await,
        "decline" => moderate(app, ctx, args, "decline").await,
        "queue" => queue(app, args.first().copied()).await,
        "upcoming" => upcoming(app, args).await,
        "missing" => missing(app, args.first().copied()).await,
        "lookup" => lookup(app, args).await,
        "health" => health(app, args.first().copied()).await,
        "users" => users(app, ctx, args).await,
        "join" => join(app, ctx, args).await,
        "part" => part(app, ctx, args).await,
        other => bail!("unhandled command {other}"),
    }
}

// ---------------------------------------------------------------------------------------

fn help(app: &App, ctx: &Ctx, which: Option<&str>) -> Vec<String> {
    let p = &app.config.bot.prefix;
    if let Some(w) = which {
        return match find(w.trim_start_matches(p.as_str())) {
            Some(s) => {
                let al = if s.aliases.is_empty() {
                    String::new()
                } else {
                    format!(" (aliases: {})", s.aliases.join(", "))
                };
                let mut lines = vec![format!(
                    "{B}{p}{}{B} — {} [{}]{al}",
                    s.usage,
                    s.summary,
                    s.role.as_str()
                )];
                if !s.examples.is_empty() {
                    let ex: Vec<String> = s.examples.iter().map(|e| format!("{p}{e}")).collect();
                    lines.push(format!("e.g. {}", ex.join("  ·  ")));
                }
                lines
            }
            None => vec![format!("no such command {w}")],
        };
    }
    let names: Vec<_> = COMMANDS
        .iter()
        .filter(|c| ctx.role >= c.role)
        .map(|c| c.name)
        .collect();
    vec![
        format!("commands: {}", names.join(" ")),
        format!(
            "{p}help <command> for details. Shorthand: {p}<service> <command>, e.g. {p}sonarr upcoming. \
             Full list: docs/COMMANDS.md in the repo, or the web panel"
        ),
    ]
}

/// Run `f` on every service concurrently, preserving configuration order.
async fn each<T, F, Fut>(svcs: Vec<Arc<Service>>, f: F) -> Vec<(Arc<Service>, Result<T>)>
where
    T: Send + 'static,
    F: Fn(Arc<Service>) -> Fut,
    Fut: Future<Output = Result<T>> + Send + 'static,
{
    let mut set = JoinSet::new();
    for (i, s) in svcs.iter().enumerate() {
        let fut = f(s.clone());
        set.spawn(async move { (i, fut.await) });
    }
    let mut out: Vec<(usize, Result<T>)> = vec![];
    while let Some(r) = set.join_next().await {
        match r {
            Ok(x) => out.push(x),
            Err(e) => out.push((usize::MAX, Err(anyhow!("task failed: {e}")))),
        }
    }
    out.sort_by_key(|(i, _)| *i);
    out.into_iter()
        .filter(|(i, _)| *i < svcs.len())
        .map(|(i, r)| (svcs[i].clone(), r))
        .collect()
}

fn pct(p: Option<f64>) -> String {
    p.map(|p| format!(" {p:.0}%")).unwrap_or_default()
}

async fn status(app: &App) -> Result<Vec<String>> {
    let irc = app.irc_status.read().await.clone();
    let up = chrono::Utc::now() - app.started;
    let mut lines = vec![format!(
        "mediabot {} up {}h{:02}m, on {} as {} in {}",
        env!("CARGO_PKG_VERSION"),
        up.num_hours(),
        up.num_minutes() % 60,
        irc.server,
        irc.nick,
        irc.channels.iter().cloned().collect::<Vec<_>>().join(" ")
    )];
    let st = app.service_status.read().await;
    if st.is_empty() {
        lines.push("no services configured".into());
        return Ok(lines);
    }
    let parts: Vec<String> = app
        .services
        .iter()
        .filter_map(|s| st.get(&s.name))
        .map(|s| {
            if s.standby {
                format!("{} ⏸ standby", s.name)
            } else if s.last_poll.is_none() {
                format!("{} …", s.name)
            } else if s.up {
                let backends = if s.backends.is_empty() {
                    String::new()
                } else {
                    let b: Vec<String> = s
                        .backends
                        .iter()
                        .map(|b| format!("{} {}", b.name, if b.up { "✓" } else { "✗" }))
                        .collect();
                    format!(" ({})", b.join(", "))
                };
                format!(
                    "{} ✓ {}{backends}",
                    s.name,
                    s.version.as_deref().unwrap_or("")
                )
            } else {
                format!(
                    "{} ✗ ({})",
                    s.name,
                    s.error
                        .as_deref()
                        .unwrap_or("down")
                        .chars()
                        .take(60)
                        .collect::<String>()
                )
            }
        })
        .collect();
    for chunk in parts.chunks(5) {
        lines.push(chunk.join(" | "));
    }
    let disabled: Vec<String> = app
        .config
        .services
        .iter()
        .filter(|s| !s.enabled)
        .map(|s| s.name())
        .collect();
    if !disabled.is_empty() {
        lines.push(format!("disabled: {}", disabled.join(", ")));
    }
    Ok(lines)
}

/// Run `f` on the primary media services; if every one fails and there's a fallback (the direct
/// servers while Tracearr is preferred), run it on the fallback and say so.
async fn media_each<T, F, Fut>(
    app: &App,
    which: Option<&str>,
    f: F,
) -> Result<(Vec<(Arc<Service>, Result<T>)>, Option<String>)>
where
    T: Send + 'static,
    F: Fn(Arc<Service>) -> Fut + Clone,
    Fut: Future<Output = Result<T>> + Send + 'static,
{
    let (primary, fallback) = app.media_targets(which).await?;
    let results = each(primary, f.clone()).await;
    if fallback.is_empty() || results.iter().any(|(_, r)| r.is_ok()) {
        return Ok((results, None));
    }
    let why = results
        .iter()
        .filter_map(|(s, r)| r.as_ref().err().map(|e| format!("{}: {e:#}", s.name)))
        .next()
        .unwrap_or_default();
    let note = format!(
        "Tracearr unavailable ({}) - asking the servers directly",
        why.chars().take(80).collect::<String>()
    );
    Ok((each(fallback, f).await, Some(note)))
}

async fn now_playing(app: &App, which: Option<&str>) -> Result<Vec<String>> {
    let (results, note) = media_each(app, which, |s| async move { s.activity().await }).await?;
    let mut lines: Vec<String> = note.into_iter().collect();
    let mut idle = vec![];
    for (svc, res) in results {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok(a) if a.streams.is_empty() => idle.push(svc.name.clone()),
            Ok(a) => {
                let tc = a.streams.iter().filter(|s| s.transcode).count();
                let bw = a
                    .bandwidth_kbps
                    .filter(|b| *b > 0.0)
                    .map(|b| format!(", {:.1} Mbps", b / 1000.0))
                    .unwrap_or_default();
                lines.push(format!(
                    "{B}{}{B}: {} stream(s), {tc} transcoding{bw}",
                    svc.name,
                    a.streams.len()
                ));
                for s in a.streams {
                    let q = s.quality.map(|q| format!("{q}, ")).unwrap_or_default();
                    let mode = if s.transcode { "transcode" } else { "direct" };
                    let state = if s.paused { "⏸" } else { "▶" };
                    let server = s.server.map(|v| format!(" · {v}")).unwrap_or_default();
                    lines.push(format!(
                        "  {state} {} — {} [{q}{mode}]{} on {}{server}",
                        s.user,
                        s.title,
                        pct(s.progress),
                        s.player
                    ));
                }
            }
        }
    }
    if !idle.is_empty() {
        lines.push(format!("nothing playing on {}", idle.join(", ")));
    }
    Ok(lines)
}

/// 179644 -> "179,644"
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

async fn libraries(app: &App, which: Option<&str>) -> Result<Vec<String>> {
    let (results, note) = media_each(app, which, |s| async move { s.libraries().await }).await?;
    let mut lines: Vec<String> = note.into_iter().collect();
    for (svc, res) in results {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            // Tracearr: per-server totals by type
            Ok(libs) if libs.iter().any(|l| l.server.is_some()) => {
                let mut servers: Vec<&str> = vec![];
                for l in &libs {
                    let s = l.server.as_deref().unwrap_or("?");
                    if !servers.contains(&s) {
                        servers.push(s);
                    }
                }
                for srv in servers {
                    let parts: Vec<String> = libs
                        .iter()
                        .filter(|l| l.server.as_deref() == Some(srv))
                        .map(|l| {
                            let c = l.count.map_or("?".into(), thousands);
                            match l.kind.as_str() {
                                "movie" => format!("{c} movies"),
                                "show" => match l.children {
                                    Some(e) => format!("{c} shows ({} eps)", thousands(e)),
                                    None => format!("{c} shows"),
                                },
                                "track" => format!("{c} tracks"),
                                _ => format!("{c} other"),
                            }
                        })
                        .collect();
                    lines.push(format!(
                        "{B}{srv}{B} (via {}): {}",
                        svc.name,
                        parts.join(" · ")
                    ));
                }
            }
            Ok(libs) => {
                let parts: Vec<String> = libs
                    .iter()
                    .map(|l| {
                        let child = match (l.children, l.kind.as_str()) {
                            (Some(c), "show") => format!(" ({c} eps)"),
                            (Some(c), "artist") => format!(" ({c} tracks)"),
                            _ => String::new(),
                        };
                        format!(
                            "{} {}{child}",
                            l.name,
                            l.count.map_or("?".into(), |c| c.to_string())
                        )
                    })
                    .collect();
                lines.push(format!("{B}{}{B}: {}", svc.name, parts.join(" · ")));
            }
        }
    }
    Ok(lines)
}

/// Tracearr services, narrowed by any argument that is a service name; returns the other args.
fn tracearr_args<'a>(app: &App, args: &[&'a str]) -> Result<(Vec<Arc<Service>>, Vec<&'a str>)> {
    let mut which = None;
    let mut rest = vec![];
    for a in args {
        if which.is_none() && app.is_service_word(a) {
            which = Some(*a);
        } else {
            rest.push(*a);
        }
    }
    let svcs = app.select(|k| k == ServiceKind::Tracearr, which, "tracearr")?;
    Ok((svcs, rest))
}

async fn today(app: &App, args: &[&str]) -> Result<Vec<String>> {
    let (svcs, _) = tracearr_args(app, args)?;
    let tz = app.config.timezone();
    let mut lines = vec![];
    for (svc, res) in each(svcs, move |s| {
        let tz = tz.clone();
        async move { s.tracearr_today(&tz).await }
    })
    .await
    {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok(t) => lines.push(format!(
                "{B}{}{B} today: {} plays ({} sessions), {:.1} h watched, {} active user(s), {} playing now{}",
                svc.name,
                t.plays,
                t.sessions,
                t.watch_hours,
                t.active_users,
                t.active_streams,
                if t.alerts > 0 { format!(", {} alert(s) in 24 h", t.alerts) } else { String::new() }
            )),
        }
    }
    Ok(lines)
}

async fn top(app: &App, args: &[&str]) -> Result<Vec<String>> {
    let (svcs, rest) = tracearr_args(app, args)?;
    let (mut kinds, mut days, mut sort) = (vec![], 7u32, "plays");
    for a in rest {
        match a.to_ascii_lowercase().as_str() {
            "movie" | "movies" | "films" | "film" => kinds.push("movie"),
            "show" | "shows" | "tv" | "series" => kinds.push("show"),
            "plays" | "play" => sort = "plays",
            "time" | "watch" | "watchtime" | "hours" => sort = "watch_time",
            "viewers" | "users" | "people" => sort = "viewers",
            other => match other.trim_end_matches('d').parse::<u32>() {
                Ok(d) => days = d.clamp(1, 365),
                Err(_) => bail!(
                    "unknown argument {other:?} (movies, shows, a number of days, plays, time or viewers)"
                ),
            },
        }
    }
    let limit = if kinds.is_empty() { 3 } else { 5 };
    if kinds.is_empty() {
        kinds = vec!["movie", "show"];
    }
    let by = match sort {
        "watch_time" => "watch time",
        s => s,
    };
    let mut lines = vec![];
    for svc in svcs {
        for kind in &kinds {
            let label = if *kind == "movie" { "movies" } else { "shows" };
            match svc.tracearr_top(kind, days, sort, limit).await {
                Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
                Ok(items) if items.is_empty() => {
                    lines.push(format!("no {label} watched in the last {days} days"))
                }
                Ok(items) => {
                    lines.push(format!("{B}Top {label}{B}, last {days} days by {by}:"));
                    for (i, t) in items.iter().enumerate() {
                        let y = t.year.map(|y| format!(" ({y})")).unwrap_or_default();
                        lines.push(format!(
                            "  {}. {}{y} — {} play(s), {:.1} h, {} viewer(s)",
                            i + 1,
                            t.title,
                            t.plays,
                            t.watch_hours,
                            t.viewers
                        ));
                    }
                }
            }
        }
    }
    Ok(lines)
}

async fn recent(app: &App, args: &[&str]) -> Result<Vec<String>> {
    let (svcs, rest) = tracearr_args(app, args)?;
    let (mut kind, mut count) = (None, 6usize);
    for a in rest {
        kind = Some(match a.to_ascii_lowercase().as_str() {
            "movie" | "movies" | "films" => "movie",
            "show" | "shows" | "series" => "show",
            "episode" | "episodes" | "eps" | "tv" => "episode",
            "music" | "album" | "albums" => "album",
            "artist" | "artists" => "artist",
            "track" | "tracks" | "songs" => "track",
            other => match other.parse::<usize>() {
                Ok(n) => {
                    count = n.clamp(1, 15);
                    continue;
                }
                Err(_) => bail!(
                    "unknown argument {other:?} (movies, shows, episodes, music, artists, tracks, or a count)"
                ),
            },
        });
    }
    let mut lines = vec![];
    for svc in svcs {
        match svc.tracearr_recent(kind, count).await {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok(items) if items.is_empty() => {
                lines.push(format!("{}: nothing added recently", svc.name))
            }
            Ok(items) => {
                lines.push(format!(
                    "{B}Recently added{B}{}:",
                    kind.map(|k| format!(" ({k}s)")).unwrap_or_default()
                ));
                for r in items {
                    let y = r.year.map(|y| format!(" ({y})")).unwrap_or_default();
                    let t = if kind.is_some() {
                        String::new()
                    } else {
                        format!(" [{}]", r.media_type)
                    };
                    lines.push(format!("  {}  {}{y}{t} · {}", r.added, r.title, r.server));
                }
            }
        }
    }
    Ok(lines)
}

fn seerr(app: &App) -> Result<Arc<Service>> {
    Ok(app
        .select(|k| k == ServiceKind::Seerr, None, "seerr")?
        .remove(0))
}

async fn search(app: &App, args: &[&str]) -> Result<Vec<String>> {
    if args.is_empty() {
        bail!("usage: search <title>");
    }
    let svc = seerr(app)?;
    let q = args.join(" ");
    let hits = svc.seerr_search(&q, 5).await?;
    if hits.is_empty() {
        return Ok(vec![format!("no results for {q:?}")]);
    }
    let p = &app.config.bot.prefix;
    let mut lines: Vec<String> = hits
        .iter()
        .map(|h| {
            let y = h
                .year
                .as_deref()
                .map(|y| format!(" ({y})"))
                .unwrap_or_default();
            let st = h.status.map(seerr::media_status).unwrap_or("not requested");
            format!(
                "[{} {}] {B}{}{B}{y} — {st}",
                h.media_type, h.tmdb_id, h.title
            )
        })
        .collect();
    lines.push(format!(
        "admins can request with {p}request <movie|tv> <id>"
    ));
    Ok(lines)
}

async fn requests(app: &App, filter: Option<&str>) -> Result<Vec<String>> {
    let filter = filter.unwrap_or("pending").to_ascii_lowercase();
    let allowed = [
        "all",
        "approved",
        "available",
        "pending",
        "processing",
        "unavailable",
        "failed",
        "completed",
        "deleted",
    ];
    if !allowed.contains(&filter.as_str()) {
        bail!("filter must be one of {}", allowed.join(", "));
    }
    let svc = seerr(app)?;
    let (total, reqs) = svc.seerr_requests(&filter, 6).await?;
    if reqs.is_empty() {
        return Ok(vec![format!("no {filter} requests")]);
    }
    let mut lines = vec![format!("{total} {filter} request(s):")];
    for r in reqs {
        lines.push(format!(
            "  #{} {B}{}{B} [{}] by {} on {} — {}",
            r.id,
            r.title,
            r.media_type,
            r.requested_by,
            r.created,
            seerr::request_status(r.status)
        ));
    }
    Ok(lines)
}

async fn request(app: &App, ctx: &Ctx, args: &[&str]) -> Result<Vec<String>> {
    let (Some(mt), Some(id)) = (args.first(), args.get(1)) else {
        bail!("usage: request <movie|tv> <tmdb-id> [seasons]");
    };
    let mt = match mt.to_ascii_lowercase().as_str() {
        "movie" | "m" => "movie",
        "tv" | "show" | "series" => "tv",
        other => bail!("type must be movie or tv, not {other:?}"),
    };
    let id: i64 = id
        .trim_start_matches('#')
        .parse()
        .map_err(|_| anyhow!("tmdb id must be a number"))?;
    let seasons: Vec<i64> = match args.get(2) {
        None => vec![],
        Some(s) if s.eq_ignore_ascii_case("all") => vec![],
        Some(s) => s
            .split(',')
            .map(|x| x.trim().parse())
            .collect::<Result<_, _>>()
            .map_err(|_| anyhow!("seasons must be like 1,2,3"))?,
    };
    let svc = seerr(app)?;
    let title = svc
        .seerr_title(mt, id)
        .await
        .unwrap_or_else(|_| format!("tmdb:{id}"));
    let r = svc.seerr_request(mt, id, &seasons).await?;
    info!(target: "audit", actor = %ctx.actor, media_type = mt, tmdb_id = id, "seerr request created");
    let rid = r["id"]
        .as_i64()
        .map(|i| format!(" #{i}"))
        .unwrap_or_default();
    let st = r["status"]
        .as_i64()
        .map(seerr::request_status)
        .unwrap_or("submitted");
    Ok(vec![format!("requested {B}{title}{B}{rid} — {st}")])
}

async fn moderate(app: &App, ctx: &Ctx, args: &[&str], action: &str) -> Result<Vec<String>> {
    let id: i64 = args
        .first()
        .ok_or_else(|| anyhow!("usage: {action} <request-id>"))?
        .trim_start_matches('#')
        .parse()
        .map_err(|_| anyhow!("request id must be a number"))?;
    seerr(app)?.seerr_moderate(id, action).await?;
    info!(target: "audit", actor = %ctx.actor, request_id = id, "seerr request {action}d");
    Ok(vec![format!("request #{id} {action}d")])
}

async fn queue(app: &App, which: Option<&str>) -> Result<Vec<String>> {
    let svcs = app.select(ServiceKind::is_arr, which, "sonarr/radarr/lidarr")?;
    let mut lines = vec![];
    let mut empty = vec![];
    for (svc, res) in each(svcs, |s| async move { s.arr_queue(5).await }).await {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok((0, _)) => empty.push(svc.name.clone()),
            Ok((total, items)) => {
                lines.push(format!("{B}{}{B}: {total} in queue", svc.name));
                for i in items {
                    let tl = i
                        .timeleft
                        .map(|t| format!(", {t} left"))
                        .unwrap_or_default();
                    lines.push(format!(
                        "  {} — {}{}{tl}",
                        i.title,
                        i.status,
                        pct(i.progress)
                    ));
                }
            }
        }
    }
    if !empty.is_empty() {
        lines.push(format!("queue empty: {}", empty.join(", ")));
    }
    Ok(lines)
}

async fn upcoming(app: &App, args: &[&str]) -> Result<Vec<String>> {
    let (mut which, mut days) = (None, 7);
    for a in args {
        match a.parse::<i64>() {
            Ok(d) => days = d.clamp(1, 60),
            Err(_) => which = Some(*a),
        }
    }
    let svcs = app.select(ServiceKind::is_arr, which, "sonarr/radarr/lidarr")?;
    let mut lines = vec![];
    for (svc, res) in each(svcs, move |s| async move { s.arr_upcoming(days).await }).await {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok(rows) if rows.is_empty() => {
                lines.push(format!("{}: nothing in the next {days} days", svc.name))
            }
            Ok(rows) => {
                lines.push(format!("{B}{}{B}: next {days} days", svc.name));
                lines.extend(rows.into_iter().take(6).map(|r| format!("  {r}")));
            }
        }
    }
    Ok(lines)
}

async fn missing(app: &App, which: Option<&str>) -> Result<Vec<String>> {
    let svcs = app.select(ServiceKind::is_arr, which, "sonarr/radarr/lidarr")?;
    let mut lines = vec![];
    for (svc, res) in each(svcs, |s| async move { s.arr_missing(5).await }).await {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok((0, _)) => lines.push(format!("{}: nothing missing", svc.name)),
            Ok((total, rows)) => {
                lines.push(format!("{B}{}{B}: {total} missing", svc.name));
                lines.extend(rows.into_iter().map(|r| format!("  {r}")));
            }
        }
    }
    Ok(lines)
}

async fn lookup(app: &App, args: &[&str]) -> Result<Vec<String>> {
    if args.len() < 2 {
        bail!("usage: lookup <sonarr|radarr|lidarr|name> <term>");
    }
    let svcs = app.select(ServiceKind::is_arr, Some(args[0]), "sonarr/radarr/lidarr")?;
    let term = args[1..].join(" ");
    let mut lines = vec![];
    for (svc, res) in each(svcs, move |s| {
        let term = term.clone();
        async move { s.arr_lookup(&term, 4).await }
    })
    .await
    {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok(rows) if rows.is_empty() => lines.push(format!("{}: no matches", svc.name)),
            Ok(rows) => lines.extend(rows.into_iter().map(|r| format!("{}: {r}", svc.name))),
        }
    }
    Ok(lines)
}

async fn health(app: &App, which: Option<&str>) -> Result<Vec<String>> {
    let svcs = app.select(ServiceKind::is_arr, which, "sonarr/radarr/lidarr")?;
    let mut lines = vec![];
    let mut ok = vec![];
    for (svc, res) in each(svcs, |s| async move { s.arr_health().await }).await {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok(h) if h.is_empty() => ok.push(svc.name.clone()),
            Ok(h) => lines.extend(
                h.into_iter()
                    .map(|i| format!("{} [{}] {}", svc.name, i.level, i.message)),
            ),
        }
    }
    if !ok.is_empty() {
        lines.push(format!("healthy: {}", ok.join(", ")));
    }
    Ok(lines)
}

async fn users(app: &App, ctx: &Ctx, args: &[&str]) -> Result<Vec<String>> {
    let sub = args
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "list".into());
    match sub.as_str() {
        "list" | "ls" => {
            let mut lines = vec![format!("owners: {}", app.store.owners().join(", "))];
            let users = app.store.users().await;
            for role in [Role::Admin, Role::ReadOnly] {
                let ids: Vec<_> = users
                    .iter()
                    .filter(|u| u.role == role)
                    .map(|u| u.id.as_str())
                    .collect();
                lines.push(format!(
                    "{}: {}",
                    role.as_str(),
                    if ids.is_empty() {
                        "-".into()
                    } else {
                        ids.join(", ")
                    }
                ));
            }
            Ok(lines)
        }
        "add" | "set" => {
            let (Some(id), Some(role)) = (args.get(1), args.get(2)) else {
                bail!("usage: users add <account|nick!user@host> <readonly|admin>");
            };
            let role =
                Role::parse(role).ok_or_else(|| anyhow!("role must be readonly or admin"))?;
            let created = app.store.set_user(id, role, &ctx.actor, ctx.role).await?;
            app.refresh_user_metrics().await;
            info!(target: "audit", actor = %ctx.actor, user = %id, role = role.as_str(), "acl user set");
            let kind = if is_mask(id) {
                "hostmask"
            } else {
                "NickServ account"
            };
            Ok(vec![format!(
                "{} {kind} {id} as {}",
                if created { "added" } else { "updated" },
                role.as_str()
            )])
        }
        "del" | "rm" | "remove" => {
            let id = args
                .get(1)
                .ok_or_else(|| anyhow!("usage: users del <account|mask>"))?;
            app.store.remove_user(id, ctx.role).await?;
            app.refresh_user_metrics().await;
            info!(target: "audit", actor = %ctx.actor, user = %id, "acl user removed");
            Ok(vec![format!("removed {id}")])
        }
        other => bail!("unknown subcommand {other:?} (list, add, del)"),
    }
}

async fn join(app: &App, ctx: &Ctx, args: &[&str]) -> Result<Vec<String>> {
    let name = args
        .first()
        .ok_or_else(|| anyhow!("usage: join <#channel> [default-role] [key]"))?;
    if !name.starts_with('#') {
        bail!("channel must start with #");
    }
    let default_role = match args.get(1) {
        None => Role::None,
        Some(r) if r.eq_ignore_ascii_case("none") => Role::None,
        Some(r) => {
            Role::parse(r).ok_or_else(|| anyhow!("default role must be none, readonly or admin"))?
        }
    };
    let ch = ChannelConfig {
        name: name.to_string(),
        key: args.get(2).map(|s| s.to_string()),
        default_role,
    };
    upsert_channel(app, ch.clone()).await?;
    app.irc.send(IrcCmd::Join(ch));
    info!(target: "audit", actor = %ctx.actor, channel = %name, "join");
    Ok(vec![format!("joining {name}")])
}

pub async fn upsert_channel(app: &App, ch: ChannelConfig) -> Result<()> {
    let mut chans = app.store.channels(&app.config.irc.channels).await;
    chans.retain(|c| irc_lower(&c.name) != irc_lower(&ch.name));
    chans.push(ch);
    app.store.set_channels(chans).await
}

pub async fn remove_channel(app: &App, name: &str) -> Result<()> {
    let mut chans = app.store.channels(&app.config.irc.channels).await;
    chans.retain(|c| irc_lower(&c.name) != irc_lower(name));
    app.store.set_channels(chans).await
}

async fn part(app: &App, ctx: &Ctx, args: &[&str]) -> Result<Vec<String>> {
    let name = args
        .first()
        .ok_or_else(|| anyhow!("usage: part <#channel>"))?;
    remove_channel(app, name).await?;
    app.irc.send(IrcCmd::Part(name.to_string()));
    info!(target: "audit", actor = %ctx.actor, channel = %name, "part");
    Ok(vec![format!("leaving {name}")])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addressed() {
        assert_eq!(strip_addressed("mediabot: np", "MediaBot"), Some("np"));
        assert_eq!(strip_addressed("mediabot, np", "mediabot"), Some("np"));
        assert_eq!(strip_addressed("mediabotx np", "mediabot"), None);
        assert_eq!(strip_addressed("é", "mediabot"), None);
    }

    #[test]
    fn truncation() {
        let v: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        let t = truncate(v, 4);
        assert_eq!(t.len(), 4);
        assert!(t[3].contains("7 more"));
    }

    #[test]
    fn command_table_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in COMMANDS {
            assert!(seen.insert(c.name), "{}", c.name);
            for a in c.aliases {
                assert!(seen.insert(a), "{a}");
            }
        }
    }

    #[test]
    fn command_table_is_consistent() {
        let media = ["plex", "emby", "jellyfin", "tautulli", "tofa", "tracearr"];
        let arrs = ["sonarr", "radarr", "lidarr"];
        for c in COMMANDS {
            assert!(
                GROUPS.contains(&c.group),
                "{}: unknown group {}",
                c.name,
                c.group
            );
            assert!(
                c.usage.starts_with(c.name),
                "{}: usage must start with the name",
                c.name
            );
            assert!(!c.examples.is_empty(), "{}: needs an example", c.name);
            for e in c.examples {
                let mut w = e.split_whitespace();
                let first = w.next().unwrap();
                // `!<service> [command]` shorthand resolves the same way execute() does
                let resolved = if let Some(s) = find(first) {
                    s.name
                } else if media.contains(&first) || arrs.contains(&first) || first == "seerr" {
                    match w.next() {
                        Some(sub) => find(sub).map(|s| s.name).unwrap_or("?"),
                        None if media.contains(&first) => "np",
                        None if first == "seerr" => "requests",
                        None => "queue",
                    }
                } else {
                    "?"
                };
                assert_eq!(resolved, c.name, "example {e:?} doesn't run {}", c.name);
            }
        }
    }

    /// docs/COMMANDS.md must match the table. Regenerate: UPDATE_DOCS=1 cargo test commands_doc
    #[test]
    fn commands_doc_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/COMMANDS.md");
        let want = commands_markdown();
        if std::env::var_os("UPDATE_DOCS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &want).unwrap();
            return;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == want,
            "docs/COMMANDS.md is out of date - run: UPDATE_DOCS=1 cargo test commands_doc"
        );
    }
}
