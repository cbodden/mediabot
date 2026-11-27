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

struct Spec {
    name: &'static str,
    aliases: &'static [&'static str],
    role: Role,
    usage: &'static str,
    help: &'static str,
}

const COMMANDS: &[Spec] = &[
    Spec {
        name: "help",
        aliases: &["commands"],
        role: Role::ReadOnly,
        usage: "help [command]",
        help: "list commands or show help for one",
    },
    Spec {
        name: "whoami",
        aliases: &[],
        role: Role::None,
        usage: "whoami",
        help: "show your identity and role",
    },
    Spec {
        name: "status",
        aliases: &["services"],
        role: Role::ReadOnly,
        usage: "status",
        help: "backend health and versions",
    },
    Spec {
        name: "np",
        aliases: &["streams", "playing", "nowplaying"],
        role: Role::ReadOnly,
        usage: "np [server]",
        help: "what's playing on plex/emby/jellyfin/tautulli",
    },
    Spec {
        name: "libs",
        aliases: &["libraries", "library"],
        role: Role::ReadOnly,
        usage: "libs [server]",
        help: "library item counts",
    },
    Spec {
        name: "search",
        aliases: &["find"],
        role: Role::ReadOnly,
        usage: "search <title>",
        help: "search Seerr (movies + TV) and show availability",
    },
    Spec {
        name: "requests",
        aliases: &["reqs"],
        role: Role::ReadOnly,
        usage: "requests [pending|approved|processing|available|failed|all]",
        help: "list Seerr requests (default: pending)",
    },
    Spec {
        name: "request",
        aliases: &["req"],
        role: Role::Admin,
        usage: "request <movie|tv> <tmdb-id> [seasons e.g. 1,2]",
        help: "create a Seerr request (ids come from !search)",
    },
    Spec {
        name: "approve",
        aliases: &[],
        role: Role::Admin,
        usage: "approve <request-id>",
        help: "approve a pending Seerr request",
    },
    Spec {
        name: "decline",
        aliases: &["deny"],
        role: Role::Admin,
        usage: "decline <request-id>",
        help: "decline a pending Seerr request",
    },
    Spec {
        name: "queue",
        aliases: &["q", "downloads"],
        role: Role::ReadOnly,
        usage: "queue [sonarr|radarr|lidarr|name]",
        help: "download queues",
    },
    Spec {
        name: "upcoming",
        aliases: &["calendar", "cal"],
        role: Role::ReadOnly,
        usage: "upcoming [service] [days]",
        help: "upcoming releases (default 7 days)",
    },
    Spec {
        name: "missing",
        aliases: &["wanted"],
        role: Role::ReadOnly,
        usage: "missing [service]",
        help: "monitored items without files",
    },
    Spec {
        name: "lookup",
        aliases: &["info"],
        role: Role::ReadOnly,
        usage: "lookup <service> <term>",
        help: "look up a series/movie/artist and whether it's in the library",
    },
    Spec {
        name: "health",
        aliases: &[],
        role: Role::ReadOnly,
        usage: "health [service]",
        help: "*arr health check warnings",
    },
    Spec {
        name: "users",
        aliases: &["user", "acl"],
        role: Role::Admin,
        usage: "users [list | add <account|mask> <readonly|admin> | del <account|mask>]",
        help: "manage who can use the bot",
    },
    Spec {
        name: "join",
        aliases: &[],
        role: Role::Owner,
        usage: "join <#channel> [default-role] [key]",
        help: "join a channel (persisted)",
    },
    Spec {
        name: "part",
        aliases: &["leave"],
        role: Role::Owner,
        usage: "part <#channel>",
        help: "leave a channel (persisted)",
    },
];

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
                vec![format!(
                    "{B}{p}{}{B} — {} [{}]{al}",
                    s.usage,
                    s.help,
                    s.role.as_str()
                )]
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
            "{p}help <command> for details. Shorthand: {p}<service> <command>, e.g. {p}sonarr upcoming"
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
            if s.last_poll.is_none() {
                format!("{} …", s.name)
            } else if s.up {
                format!("{} ✓ {}", s.name, s.version.as_deref().unwrap_or(""))
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
    Ok(lines)
}

async fn now_playing(app: &App, which: Option<&str>) -> Result<Vec<String>> {
    let svcs = app.select(ServiceKind::is_media_server, which, "media server")?;
    let mut lines = vec![];
    let mut idle = vec![];
    for (svc, res) in each(svcs, |s| async move { s.activity().await }).await {
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
                    lines.push(format!(
                        "  {state} {} — {} [{q}{mode}]{} on {}",
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

async fn libraries(app: &App, which: Option<&str>) -> Result<Vec<String>> {
    let svcs = app.select(ServiceKind::is_media_server, which, "media server")?;
    let mut lines = vec![];
    for (svc, res) in each(svcs, |s| async move { s.libraries().await }).await {
        match res {
            Err(e) => lines.push(format!("{}: error: {e:#}", svc.name)),
            Ok(libs) => {
                let parts: Vec<String> = libs
                    .iter()
                    .map(|l| {
                        let child = match (l.children, l.kind.as_str()) {
                            (Some(c), "show") => format!(" ({c} eps)"),
                            (Some(c), "artist") => format!(" ({c} tracks)"),
                            _ => String::new(),
                        };
                        format!("{} {}{child}", l.name, l.count)
                    })
                    .collect();
                lines.push(format!("{B}{}{B}: {}", svc.name, parts.join(" · ")));
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
}
