//! Minimal, purpose-built IRC client for Libera.Chat: TLS, IRCv3 CAP + SASL PLAIN,
//! `account-tag` for identity, flood control, ping timeouts and reconnect with backoff.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::mpsc,
};
use tracing::{debug, error, info, warn};

use crate::{
    acl::{Sender, irc_lower},
    app::App,
    config::{ChannelConfig, IrcConfig},
};

// ---------------------------------------------------------------------------------------
// Public handle + status
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum IrcCmd {
    Privmsg { target: String, text: String },
    Join(ChannelConfig),
    Part(String),
    Raw(String),
    Reconnect,
}

#[derive(Clone)]
pub struct IrcHandle {
    tx: mpsc::UnboundedSender<IrcCmd>,
}

impl IrcHandle {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<IrcCmd>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
    pub fn send(&self, cmd: IrcCmd) {
        let _ = self.tx.send(cmd);
    }
    pub fn say(&self, target: &str, text: impl Into<String>) {
        self.send(IrcCmd::Privmsg {
            target: target.to_string(),
            text: text.into(),
        });
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct IrcStatus {
    pub connected: bool,
    pub server: String,
    pub nick: String,
    pub account: Option<String>,
    pub channels: BTreeSet<String>,
    pub caps: Vec<String>,
    pub connected_since: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// An incoming chat message for the command layer.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub sender: Sender,
    /// Channel name, or None for a private message.
    pub channel: Option<String>,
    pub text: String,
}

impl ChatMessage {
    /// Where replies go: the channel, or the sender's nick for PMs.
    pub fn reply_target(&self) -> &str {
        self.channel.as_deref().unwrap_or(&self.sender.nick)
    }
}

// ---------------------------------------------------------------------------------------
// Protocol parsing
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
pub struct Message {
    pub tags: HashMap<String, String>,
    pub prefix: Option<String>,
    pub command: String,
    pub params: Vec<String>,
}

fn unescape_tag(v: &str) -> String {
    let mut out = String::new();
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some(':') => out.push(';'),
                Some('s') => out.push(' '),
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl Message {
    pub fn parse(line: &str) -> Option<Message> {
        let mut rest = line.trim_end_matches(['\r', '\n']);
        let mut m = Message::default();
        if let Some(r) = rest.strip_prefix('@') {
            let (tags, r) = r.split_once(' ')?;
            for t in tags.split(';') {
                let (k, v) = t.split_once('=').unwrap_or((t, ""));
                m.tags.insert(k.to_string(), unescape_tag(v));
            }
            rest = r.trim_start_matches(' ');
        }
        if let Some(r) = rest.strip_prefix(':') {
            let (p, r) = r.split_once(' ')?;
            m.prefix = Some(p.to_string());
            rest = r.trim_start_matches(' ');
        }
        let (cmd, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
        if cmd.is_empty() {
            return None;
        }
        m.command = cmd.to_ascii_uppercase();
        loop {
            rest = rest.trim_start_matches(' ');
            if rest.is_empty() {
                break;
            }
            if let Some(trailing) = rest.strip_prefix(':') {
                m.params.push(trailing.to_string());
                break;
            }
            let (p, r) = rest.split_once(' ').unwrap_or((rest, ""));
            m.params.push(p.to_string());
            rest = r;
        }
        Some(m)
    }

    fn param(&self, i: usize) -> &str {
        self.params.get(i).map(String::as_str).unwrap_or("")
    }

    fn source_nick(&self) -> &str {
        self.prefix
            .as_deref()
            .map(|p| p.split('!').next().unwrap_or(p))
            .unwrap_or("")
    }

    fn sender(&self) -> Sender {
        let p = self.prefix.as_deref().unwrap_or("");
        let (nick, rest) = p.split_once('!').unwrap_or((p, ""));
        let (user, host) = rest.split_once('@').unwrap_or((rest, ""));
        Sender {
            nick: nick.into(),
            user: user.into(),
            host: host.into(),
            account: self
                .tags
                .get("account")
                .filter(|a| !a.is_empty() && *a != "*")
                .cloned(),
        }
    }
}

/// Strip CR/LF/NUL and split `text` into chunks of at most `max` bytes, on char
/// boundaries, preferring to break at whitespace.
pub fn split_message(text: &str, max: usize) -> Vec<String> {
    let mut out = vec![];
    for line in text.split(['\r', '\n']) {
        let line: String = line.chars().filter(|c| *c != '\0').collect();
        let mut rest = line.as_str();
        while rest.len() > max {
            let mut cut = max;
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            if let Some(sp) = rest[..cut].rfind(' ').filter(|&i| i > max / 2) {
                cut = sp;
            }
            out.push(rest[..cut].to_string());
            rest = rest[cut..].trim_start_matches(' ');
        }
        if !rest.is_empty() {
            out.push(rest.to_string());
        }
    }
    out
}

fn is_channel(t: &str) -> bool {
    t.starts_with(['#', '&', '!', '+'])
}

/// Reject anything that could inject extra protocol lines or parameters.
fn valid_target(t: &str) -> bool {
    !t.is_empty() && !t.starts_with(':') && !t.contains([' ', ',', '\r', '\n', '\0'])
}

// ---------------------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------------------

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

async fn connect(cfg: &IrcConfig) -> Result<Box<dyn Io>> {
    let tcp = tokio::time::timeout(
        Duration::from_secs(20),
        TcpStream::connect((cfg.server.as_str(), cfg.port)),
    )
    .await
    .context("connect timeout")??;
    tcp.set_nodelay(true).ok();
    if !cfg.tls {
        return Ok(Box::new(tcp));
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(cfg.server.clone())?;
    let tls = tokio_rustls::TlsConnector::from(Arc::new(tls_cfg))
        .connect(name, tcp)
        .await
        .context("TLS handshake")?;
    Ok(Box::new(tls))
}

/// Outgoing writer with a token-bucket flood limiter. `urgent` lines (PONG, registration)
/// skip the limiter.
async fn writer(
    mut w: tokio::io::WriteHalf<Box<dyn Io>>,
    mut urgent: mpsc::UnboundedReceiver<String>,
    mut normal: mpsc::UnboundedReceiver<String>,
    burst: u32,
    interval: Duration,
    app: Arc<App>,
) -> Result<()> {
    let mut virt = Instant::now();
    let window = interval * burst.max(1);
    loop {
        let line = tokio::select! {
            biased;
            l = urgent.recv() => match l { Some(l) => l, None => return Ok(()) },
            l = normal.recv() => {
                let Some(l) = l else { return Ok(()) };
                let now = Instant::now();
                virt = virt.max(now);
                if virt > now + window {
                    tokio::time::sleep(virt - (now + window)).await;
                }
                virt += interval;
                l
            }
        };
        if line.starts_with("AUTHENTICATE ") && line != "AUTHENTICATE PLAIN" {
            debug!(target: "irc::out", "AUTHENTICATE <redacted>");
        } else {
            debug!(target: "irc::out", "{line}");
        }
        app.metrics.irc_lines.with_label_values(&["out"]).inc();
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\r\n").await?;
        w.flush().await?;
    }
}

struct Session {
    app: Arc<App>,
    urgent: mpsc::UnboundedSender<String>,
    normal: mpsc::UnboundedSender<String>,
    nick: String,
    registered: bool,
    sasl_done: bool,
    cap_ls: Vec<String>,
    caps: Vec<String>,
    events: mpsc::UnboundedSender<ChatMessage>,
}

impl Session {
    fn urgent(&self, l: impl Into<String>) {
        let _ = self.urgent.send(l.into());
    }
    fn send(&self, l: impl Into<String>) {
        let _ = self.normal.send(l.into());
    }

    fn cfg(&self) -> &IrcConfig {
        &self.app.config.irc
    }

    fn privmsg(&self, verb: &str, target: &str, text: &str) {
        if !valid_target(target) {
            warn!("refusing to send to invalid target {target:?}");
            return;
        }
        // Leave room for ":nick!user@host VERB target :" as the server relays it (max 512).
        let overhead =
            1 + self.nick.len() + 1 + 10 + 1 + 63 + 1 + verb.len() + 1 + target.len() + 2 + 2;
        let max = 510usize.saturating_sub(overhead).max(64);
        for chunk in split_message(text, max) {
            self.send(format!("{verb} {target} :{chunk}"));
        }
    }

    async fn join(&self, ch: &ChannelConfig) {
        if !is_channel(&ch.name) || !valid_target(&ch.name) {
            warn!("invalid channel name {:?}", ch.name);
            return;
        }
        match &ch.key {
            Some(k) if !k.is_empty() && !k.contains([' ', '\r', '\n']) => {
                self.send(format!("JOIN {} {k}", ch.name))
            }
            _ => self.send(format!("JOIN {}", ch.name)),
        }
    }

    async fn handle_cmd(&mut self, cmd: IrcCmd) -> Result<()> {
        match cmd {
            IrcCmd::Privmsg { target, text } => self.privmsg("PRIVMSG", &target, &text),
            IrcCmd::Join(ch) => {
                if self.registered {
                    self.join(&ch).await;
                }
            }
            IrcCmd::Part(ch) => {
                if valid_target(&ch) {
                    self.send(format!("PART {ch} :bye"));
                }
            }
            IrcCmd::Raw(l) => {
                if !l.contains(['\r', '\n', '\0']) {
                    self.send(l);
                }
            }
            IrcCmd::Reconnect => {
                self.urgent("QUIT :reconnecting");
                tokio::time::sleep(Duration::from_millis(300)).await;
                bail!("reconnect requested");
            }
        }
        Ok(())
    }

    async fn finish_cap(&mut self) {
        if !self.sasl_done {
            self.sasl_done = true;
            self.urgent("CAP END");
        }
    }

    async fn handle_line(&mut self, m: Message) -> Result<()> {
        match m.command.as_str() {
            "PING" => self.urgent(format!("PONG :{}", m.param(0))),
            "CAP" => {
                let sub = m.param(1).to_ascii_uppercase();
                match sub.as_str() {
                    "LS" => {
                        let (more, list) = if m.param(2) == "*" {
                            (true, m.param(3))
                        } else {
                            (false, m.param(2))
                        };
                        self.cap_ls.extend(
                            list.split_whitespace()
                                .map(|c| c.split('=').next().unwrap_or(c).to_string()),
                        );
                        if !more {
                            let want_sasl = self.cfg().sasl_password.is_some();
                            let wanted: Vec<&str> =
                                ["account-tag", "multi-prefix", "message-tags", "sasl"]
                                    .into_iter()
                                    .filter(|c| *c != "sasl" || want_sasl)
                                    .filter(|c| self.cap_ls.iter().any(|a| a == c))
                                    .collect();
                            if want_sasl && !wanted.contains(&"sasl") && self.cfg().sasl_required {
                                bail!("server does not offer SASL but sasl_required = true");
                            }
                            if wanted.is_empty() {
                                self.finish_cap().await;
                            } else {
                                self.urgent(format!("CAP REQ :{}", wanted.join(" ")));
                            }
                        }
                    }
                    "ACK" => {
                        self.caps
                            .extend(m.param(2).split_whitespace().map(String::from));
                        if self.caps.iter().any(|c| c == "sasl")
                            && self.cfg().sasl_password.is_some()
                        {
                            self.urgent("AUTHENTICATE PLAIN");
                        } else {
                            self.finish_cap().await;
                        }
                    }
                    "NAK" => {
                        warn!("CAP NAK: {}", m.param(2));
                        if self.cfg().sasl_required && self.cfg().sasl_password.is_some() {
                            bail!("SASL capability refused");
                        }
                        self.finish_cap().await;
                    }
                    _ => {}
                }
            }
            "AUTHENTICATE" if m.param(0) == "+" => {
                let c = self.cfg();
                let acct = c.sasl_account.clone().unwrap_or_else(|| c.nick.clone());
                let pw = c.sasl_password.clone().unwrap_or_default();
                let payload = base64::engine::general_purpose::STANDARD
                    .encode(format!("{acct}\0{acct}\0{pw}"));
                self.urgent(format!("AUTHENTICATE {payload}"));
            }
            "900" => {
                let acct = m.param(2).to_string();
                info!("logged in as account {acct}");
                self.app.irc_status.write().await.account = Some(acct);
            }
            "903" => {
                info!("SASL authentication successful");
                self.finish_cap().await;
            }
            "902" | "904" | "905" | "906" | "908" => {
                let msg = format!(
                    "SASL failed ({}): {}",
                    m.command,
                    m.params.last().map(String::as_str).unwrap_or("")
                );
                if self.cfg().sasl_required {
                    bail!(msg);
                }
                warn!("{msg}; continuing without authentication");
                self.finish_cap().await;
            }
            "001" => {
                self.registered = true;
                self.nick = m.param(0).to_string();
                info!("registered on {} as {}", self.cfg().server, self.nick);
                {
                    let mut st = self.app.irc_status.write().await;
                    st.connected = true;
                    st.nick = self.nick.clone();
                    st.caps = self.caps.clone();
                    st.connected_since = Some(Utc::now());
                    st.last_error = None;
                }
                self.app.metrics.irc_connected.set(1);
                // Libera: take our nick back from a ghost if we're authenticated.
                if irc_lower(&self.nick) != irc_lower(&self.cfg().nick)
                    && self.caps.iter().any(|c| c == "sasl")
                {
                    self.send(format!("PRIVMSG NickServ :REGAIN {}", self.cfg().nick));
                }
                for ch in self.app.store.channels(&self.app.config.irc.channels).await {
                    self.join(&ch).await;
                }
            }
            "433" | "432" if !self.registered => {
                self.nick = format!("{}_", self.nick);
                if self.nick.len() > 16 {
                    self.nick = format!(
                        "{}{}",
                        &self.cfg().nick[..self.cfg().nick.len().min(12)],
                        rand_suffix()
                    );
                }
                warn!("nick in use; trying {}", self.nick);
                self.urgent(format!("NICK {}", self.nick));
            }
            "NICK" => {
                if irc_lower(m.source_nick()) == irc_lower(&self.nick) {
                    self.nick = m.param(0).to_string();
                    self.app.irc_status.write().await.nick = self.nick.clone();
                    info!("nick is now {}", self.nick);
                }
            }
            "JOIN" => {
                if irc_lower(m.source_nick()) == irc_lower(&self.nick) {
                    info!("joined {}", m.param(0));
                    let mut st = self.app.irc_status.write().await;
                    st.channels.insert(m.param(0).to_string());
                    self.app.metrics.irc_channels.set(st.channels.len() as i64);
                }
            }
            "PART" => {
                if irc_lower(m.source_nick()) == irc_lower(&self.nick) {
                    info!("left {}", m.param(0));
                    self.remove_channel(m.param(0)).await;
                }
            }
            "KICK" => {
                if irc_lower(m.param(1)) == irc_lower(&self.nick) {
                    let ch = m.param(0).to_string();
                    warn!("kicked from {ch} by {}: {}", m.source_nick(), m.param(2));
                    self.remove_channel(&ch).await;
                    if self.cfg().rejoin_on_kick {
                        let wanted = self.app.store.channels(&self.app.config.irc.channels).await;
                        if let Some(cc) = wanted
                            .into_iter()
                            .find(|c| irc_lower(&c.name) == irc_lower(&ch))
                        {
                            let h = self.app.irc.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(10)).await;
                                h.send(IrcCmd::Join(cc));
                            });
                        }
                    }
                }
            }
            "471" | "473" | "474" | "475" | "477" | "403" | "405" => {
                warn!(
                    "cannot join {}: {}",
                    m.param(1),
                    m.params.last().map(String::as_str).unwrap_or("")
                );
            }
            "PRIVMSG" => {
                let target = m.param(0);
                let text = m.param(1);
                let sender = m.sender();
                if let Some(ctcp) = text.strip_prefix('\x01') {
                    let ctcp = ctcp.trim_end_matches('\x01');
                    let (verb, arg) = ctcp.split_once(' ').unwrap_or((ctcp, ""));
                    match verb.to_ascii_uppercase().as_str() {
                        "VERSION" => self.send(format!(
                            "NOTICE {} :\x01VERSION mediabot {} (Rust)\x01",
                            sender.nick,
                            env!("CARGO_PKG_VERSION")
                        )),
                        "PING" if arg.len() < 64 => {
                            self.send(format!("NOTICE {} :\x01PING {arg}\x01", sender.nick))
                        }
                        _ => {}
                    }
                    return Ok(());
                }
                let channel = is_channel(target).then(|| target.to_string());
                let _ = self.events.send(ChatMessage {
                    sender,
                    channel,
                    text: text.to_string(),
                });
            }
            "ERROR" => bail!("server closed link: {}", m.param(0)),
            _ => {}
        }
        Ok(())
    }

    async fn remove_channel(&self, ch: &str) {
        let mut st = self.app.irc_status.write().await;
        st.channels.retain(|c| irc_lower(c) != irc_lower(ch));
        self.app.metrics.irc_channels.set(st.channels.len() as i64);
    }
}

fn rand_suffix() -> String {
    format!("{:03}", (Utc::now().timestamp_subsec_nanos() / 1000) % 1000)
}

async fn session(
    app: &Arc<App>,
    cmds: &mut mpsc::UnboundedReceiver<IrcCmd>,
    events: &mpsc::UnboundedSender<ChatMessage>,
) -> Result<()> {
    let cfg = app.config.irc.clone();
    info!(
        "connecting to {}:{} (tls={})",
        cfg.server, cfg.port, cfg.tls
    );
    let stream = connect(&cfg).await?;
    let (r, w) = tokio::io::split(stream);
    let (utx, urx) = mpsc::unbounded_channel();
    let (ntx, nrx) = mpsc::unbounded_channel();
    let writer_task = tokio::spawn(writer(
        w,
        urx,
        nrx,
        cfg.flood_burst,
        Duration::from_millis(cfg.flood_interval_ms),
        app.clone(),
    ));
    let mut s = Session {
        app: app.clone(),
        urgent: utx,
        normal: ntx,
        nick: cfg.nick.clone(),
        registered: false,
        sasl_done: false,
        cap_ls: vec![],
        caps: vec![],
        events: events.clone(),
    };
    s.urgent("CAP LS 302");
    s.urgent(format!("NICK {}", cfg.nick));
    s.urgent(format!(
        "USER {} 0 * :{}",
        cfg.username.as_deref().unwrap_or(&cfg.nick),
        cfg.realname
            .as_deref()
            .unwrap_or("mediabot - media server stats")
    ));

    let mut reader = BufReader::new(r);
    let mut buf = Vec::with_capacity(1024);
    let mut last_rx = Instant::now();
    let mut pinged = false;
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    let started = Instant::now();

    let result: Result<()> = async {
        loop {
            tokio::select! {
                n = reader.read_until(b'\n', &mut buf) => {
                    let n = n?;
                    if n == 0 {
                        bail!("connection closed by server");
                    }
                    last_rx = Instant::now();
                    pinged = false;
                    let line = String::from_utf8_lossy(&buf).into_owned();
                    buf.clear();
                    app.metrics.irc_lines.with_label_values(&["in"]).inc();
                    debug!(target: "irc::in", "{}", line.trim_end());
                    if let Some(m) = Message::parse(&line) {
                        s.handle_line(m).await?;
                    }
                }
                Some(cmd) = cmds.recv() => s.handle_cmd(cmd).await?,
                _ = tick.tick() => {
                    let idle = last_rx.elapsed();
                    if !s.registered && started.elapsed() > Duration::from_secs(90) {
                        bail!("registration timed out");
                    }
                    if idle > Duration::from_secs(240) {
                        bail!("ping timeout ({}s)", idle.as_secs());
                    }
                    if idle > Duration::from_secs(120) && !pinged {
                        s.urgent(format!("PING :{}", Utc::now().timestamp()));
                        pinged = true;
                    }
                }
            }
            if writer_task.is_finished() {
                bail!("writer stopped");
            }
        }
    }
    .await;
    writer_task.abort();
    result
}

/// Run forever, reconnecting with exponential backoff.
pub async fn run(
    app: Arc<App>,
    mut cmds: mpsc::UnboundedReceiver<IrcCmd>,
    events: mpsc::UnboundedSender<ChatMessage>,
) {
    let mut backoff = Duration::from_secs(5);
    loop {
        app.metrics.irc_reconnects.inc();
        app.irc_status.write().await.server =
            format!("{}:{}", app.config.irc.server, app.config.irc.port);
        let started = Instant::now();
        let err = session(&app, &mut cmds, &events)
            .await
            .err()
            .unwrap_or_else(|| anyhow!("session ended"));
        {
            let mut st = app.irc_status.write().await;
            st.connected = false;
            st.channels.clear();
            st.connected_since = None;
            st.last_error = Some(format!("{err:#}"));
        }
        app.metrics.irc_connected.set(0);
        app.metrics.irc_channels.set(0);
        let manual = err.to_string() == "reconnect requested";
        if started.elapsed() > Duration::from_secs(300) || manual {
            backoff = Duration::from_secs(if manual { 1 } else { 5 });
        }
        if manual {
            info!("reconnecting on request");
        } else {
            error!(
                "IRC disconnected: {err:#}; reconnecting in {}s",
                backoff.as_secs()
            );
        }
        // Drain control commands while waiting so a "reconnect" press isn't queued forever.
        let deadline = tokio::time::sleep(backoff);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                c = cmds.recv() => match c {
                    Some(IrcCmd::Reconnect) => break,
                    Some(_) => {} // can't deliver while offline
                    None => return,
                }
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tags_prefix_params() {
        let m = Message::parse("@account=alice;time=2026-01-01T00:00:00.000Z :alice!~a@user/alice PRIVMSG #media :!np hello world\r\n").unwrap();
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.params, vec!["#media", "!np hello world"]);
        let s = m.sender();
        assert_eq!(s.nick, "alice");
        assert_eq!(s.host, "user/alice");
        assert_eq!(s.account.as_deref(), Some("alice"));

        let m = Message::parse("CAP * LS * :sasl=PLAIN,EXTERNAL account-tag").unwrap();
        assert_eq!(
            m.params,
            vec!["*", "LS", "*", "sasl=PLAIN,EXTERNAL account-tag"]
        );
        let m = Message::parse("PING :irc.libera.chat").unwrap();
        assert_eq!(m.param(0), "irc.libera.chat");
        assert_eq!(Message::parse("@a=b\\sc x").unwrap().tags["a"], "b c");
    }

    #[test]
    fn no_account_tag_means_none() {
        let m = Message::parse(":bob!b@h PRIVMSG #c :hi").unwrap();
        assert!(m.sender().account.is_none());
    }

    #[test]
    fn splitting() {
        let parts = split_message("a\r\nQUIT :evil", 100);
        assert_eq!(parts, vec!["a", "QUIT :evil"]); // separate PRIVMSGs, never a raw line
        let long = "word ".repeat(100);
        for p in split_message(&long, 60) {
            assert!(p.len() <= 60);
        }
        let uni = "é".repeat(100);
        for p in split_message(&uni, 61) {
            assert!(p.len() <= 61);
        }
    }

    #[test]
    fn targets() {
        assert!(valid_target("#media"));
        assert!(valid_target("nick"));
        assert!(!valid_target("#a b"));
        assert!(!valid_target("a,b"));
        assert!(!valid_target("x\r\nQUIT"));
    }
}
