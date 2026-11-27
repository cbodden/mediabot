//! Configuration file (TOML) with `${ENV_VAR}` expansion for secrets.

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::acl::Role;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub irc: IrcConfig,
    #[serde(default)]
    pub bot: BotConfig,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default, rename = "service")]
    pub services: Vec<ServiceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrcConfig {
    #[serde(default = "d_server")]
    pub server: String,
    #[serde(default = "d_port")]
    pub port: u16,
    #[serde(default = "d_true")]
    pub tls: bool,
    pub nick: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub realname: Option<String>,
    /// NickServ account for SASL PLAIN (defaults to nick).
    #[serde(default)]
    pub sasl_account: Option<String>,
    #[serde(default)]
    pub sasl_password: Option<String>,
    /// Abort the connection if SASL fails instead of continuing unidentified.
    #[serde(default = "d_true")]
    pub sasl_required: bool,
    #[serde(default)]
    pub channels: Vec<ChannelConfig>,
    /// Outgoing flood control: burst size and steady-state interval.
    #[serde(default = "d_burst")]
    pub flood_burst: u32,
    #[serde(default = "d_flood_ms")]
    pub flood_interval_ms: u64,
    #[serde(default = "d_true")]
    pub rejoin_on_kick: bool,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Role granted to anyone speaking in this channel who has no ACL entry.
    #[serde(default)]
    pub default_role: Role,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotConfig {
    #[serde(default = "d_prefix")]
    pub prefix: String,
    /// NickServ accounts that are permanent owners (cannot be removed at runtime).
    #[serde(default)]
    pub owners: Vec<String>,
    /// Role for users with no ACL entry in private messages.
    #[serde(default)]
    pub default_role: Role,
    /// Maximum lines in a single reply before truncating.
    #[serde(default = "d_max_lines")]
    pub max_lines: usize,
    #[serde(default = "d_data_dir")]
    pub data_dir: PathBuf,
    /// Seconds between fast polls (sessions, queues) for metrics.
    #[serde(default = "d_poll")]
    pub poll_interval_secs: u64,
    /// Library counts / missing totals are polled every N fast polls.
    #[serde(default = "d_slow_every")]
    pub slow_poll_every: u64,
    /// Per-user command cooldown in milliseconds (admins/owners exempt).
    #[serde(default = "d_cooldown")]
    pub cooldown_ms: u64,
    /// Where `np` / `libs` get their answers when no service is named. See [`MediaSource`].
    #[serde(default)]
    pub media_source: MediaSource,
    /// IANA time zone for "today" in Tracearr stats (default: $TZ, else UTC).
    #[serde(default)]
    pub timezone: Option<String>,
}

/// Which media services answer `np` / `libs` and get polled for stream metrics.
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum MediaSource {
    /// Tracearr while an enabled Tracearr service is up (the direct servers go on standby and
    /// aren't polled); the direct servers whenever it isn't.
    #[default]
    Auto,
    /// Only Tracearr.
    Tracearr,
    /// Only the direct servers (Plex, Emby, Jellyfin, Tofa, Tautulli); Tracearr is still used
    /// by `today`, `top` and `recent`.
    Direct,
    /// Everything (the same stream can show twice).
    All,
}

impl Default for BotConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default = "d_listen")]
    pub listen: SocketAddr,
    #[serde(default = "d_admin")]
    pub username: String,
    /// Required when the panel is enabled.
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    /// Require basic auth for /metrics too.
    #[serde(default)]
    pub metrics_auth: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub log_level: Option<String>,
    #[serde(default)]
    pub json_logs: bool,
    #[serde(default)]
    pub loki: Option<LokiConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LokiConfig {
    /// Base URL of Loki, e.g. https://loki.example:3100 (the /loki/api/v1/push path is added
    /// if missing).
    pub url: String,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tenant: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Plex,
    Emby,
    Jellyfin,
    Tautulli,
    Tofa,
    Tracearr,
    Seerr,
    Sonarr,
    Radarr,
    Lidarr,
}

impl ServiceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plex => "plex",
            Self::Emby => "emby",
            Self::Jellyfin => "jellyfin",
            Self::Tautulli => "tautulli",
            Self::Tofa => "tofa",
            Self::Tracearr => "tracearr",
            Self::Seerr => "seerr",
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
            Self::Lidarr => "lidarr",
        }
    }
    /// Anything that can answer `np` / `libs`: a direct server or Tracearr.
    pub fn is_media_server(self) -> bool {
        self.is_direct_media() || self == Self::Tracearr
    }
    /// The servers Tracearr can stand in for.
    pub fn is_direct_media(self) -> bool {
        matches!(
            self,
            Self::Plex | Self::Emby | Self::Jellyfin | Self::Tautulli | Self::Tofa
        )
    }
    pub fn is_arr(self) -> bool {
        matches!(self, Self::Sonarr | Self::Radarr | Self::Lidarr)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// `false` skips the block entirely (its `${VAR}`s may then be unset). Default true.
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// Unique name used in commands and metric labels (defaults to the kind).
    #[serde(default)]
    pub name: Option<String>,
    pub kind: ServiceKind,
    pub url: String,
    pub api_key: String,
    #[serde(default = "d_true")]
    pub verify_tls: bool,
    #[serde(default = "d_timeout")]
    pub timeout_secs: u64,
}

impl Config {
    /// The `[[service]]` blocks that are enabled.
    pub fn enabled_services(&self) -> impl Iterator<Item = &ServiceConfig> {
        self.services.iter().filter(|s| s.enabled)
    }

    /// Time zone for Tracearr's "today" stats.
    pub fn timezone(&self) -> String {
        self.bot
            .timezone
            .clone()
            .or_else(|| env_nonempty("TZ"))
            .unwrap_or_else(|| "UTC".into())
    }
}

impl ServiceConfig {
    pub fn name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.kind.as_str().to_string())
    }
}

fn d_server() -> String {
    "irc.libera.chat".into()
}
fn d_port() -> u16 {
    6697
}
fn d_true() -> bool {
    true
}
fn d_burst() -> u32 {
    4
}
fn d_flood_ms() -> u64 {
    1500
}
fn d_prefix() -> String {
    "!".into()
}
fn d_max_lines() -> usize {
    8
}
fn d_data_dir() -> PathBuf {
    "data".into()
}
fn d_poll() -> u64 {
    30
}
fn d_slow_every() -> u64 {
    20
}
fn d_cooldown() -> u64 {
    2000
}
fn d_listen() -> SocketAddr {
    "127.0.0.1:8088".parse().unwrap()
}
fn d_admin() -> String {
    "admin".into()
}
fn d_timeout() -> u64 {
    10
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Line indexes inside `[[service]]` blocks that say `enabled = false`. A block runs from its
/// `[[service]]` header to the next `[` header.
fn disabled_service_lines(raw: &str) -> std::collections::HashSet<usize> {
    let lines: Vec<&str> = raw.lines().collect();
    let mut out = std::collections::HashSet::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("[[service]]") {
            let start = i;
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with('[') {
                i += 1;
            }
            let disabled = lines[start..i].iter().any(|l| {
                let code = l.split('#').next().unwrap_or("");
                let mut kv = code.splitn(2, '=');
                kv.next().map(str::trim) == Some("enabled")
                    && kv.next().map(str::trim) == Some("false")
            });
            if disabled {
                out.extend(start..i);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Replace `${NAME}` with the environment variable NAME. Missing variables are an error
/// so a typo never silently produces an empty password - except with `lenient` (lines of a
/// disabled `[[service]]`), where they become empty.
fn expand_env(input: &str, lenient: bool) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').context("unterminated ${ in config")?;
        let name = &after[..end];
        let val = match std::env::var(name) {
            Ok(v) => v,
            Err(_) if lenient => String::new(),
            Err(_) => bail!("config references unset env var {name}"),
        };
        out.push_str(&val);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut cfg = Self::parse(&raw)?;
        cfg.apply_env_overrides()?;
        Ok(cfg)
    }

    /// Deployment overrides that beat the file, so a container image can set sane paths
    /// (`/data`, listen on all interfaces) without the user editing their config.
    fn apply_env_overrides(&mut self) -> Result<()> {
        if let Some(v) = env_nonempty("MEDIABOT_WEB_LISTEN") {
            self.web.listen = v
                .parse()
                .with_context(|| format!("MEDIABOT_WEB_LISTEN={v:?}"))?;
        }
        if let Some(v) = env_nonempty("MEDIABOT_DATA_DIR") {
            self.bot.data_dir = v.into();
        }
        Ok(())
    }

    pub fn parse(raw: &str) -> Result<Self> {
        // Expand env only outside comment lines so commented examples don't need vars set.
        let disabled = disabled_service_lines(raw);
        let mut expanded = String::new();
        for (i, line) in raw.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                expanded.push_str(line);
            } else {
                expanded.push_str(&expand_env(line, disabled.contains(&i))?);
            }
            expanded.push('\n');
        }
        let cfg: Config = toml::from_str(&expanded).context("parsing config")?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let mut names = std::collections::HashSet::new();
        for s in &self.services {
            let n = s.name();
            if !names.insert(n.clone()) {
                bail!("duplicate service name {n:?}; set distinct `name`s");
            }
            if n.is_empty() || n.contains(char::is_whitespace) {
                bail!("service name {n:?} must be non-empty without whitespace");
            }
            if s.enabled {
                url::Url::parse(&s.url).with_context(|| format!("service {n}: bad url"))?;
            }
        }
        if let Some(tz) = &self.bot.timezone
            && (tz.is_empty() || tz.contains(char::is_whitespace))
        {
            bail!("[bot] timezone must be an IANA name like America/New_York");
        }
        if self.web.enabled && self.web.password.as_deref().unwrap_or("").is_empty() {
            bail!("[web] password is required when the web panel is enabled");
        }
        if self.web.tls_cert.is_some() != self.web.tls_key.is_some() {
            bail!("[web] tls_cert and tls_key must be set together");
        }
        if self.bot.prefix.is_empty() {
            bail!("[bot] prefix must not be empty");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_file() {
        unsafe {
            std::env::set_var("MEDIABOT_WEB_PASSWORD", "x");
            std::env::set_var("MEDIABOT_SASL_PASSWORD", "x");
        }
        let raw = include_str!("../config.example.toml");
        // Every ${VAR} referenced by the example must be set for this test.
        let mut vars = vec![];
        for line in raw.lines().filter(|l| !l.trim_start().starts_with('#')) {
            let mut rest = line;
            while let Some(i) = rest.find("${") {
                let end = rest[i..].find('}').unwrap() + i;
                vars.push(rest[i + 2..end].to_string());
                rest = &rest[end..];
            }
        }
        for v in vars {
            unsafe { std::env::set_var(v, "x") };
        }
        Config::parse(raw).unwrap();
    }

    #[test]
    fn env_and_defaults() {
        unsafe {
            std::env::set_var("MEDIABOT_TEST_PW", "s3cret");
        }
        let cfg = Config::parse(
            r##"
            [irc]
            nick = "bot"
            sasl_password = "${MEDIABOT_TEST_PW}"
            channels = [{ name = "#media", default_role = "readonly" }]
            [web]
            password = "x"
            [[service]]
            kind = "sonarr"
            url = "http://sonarr:8989"
            api_key = "k"
            [[service]]
            kind = "sonarr"
            name = "sonarr4k"
            url = "http://sonarr4k:8989"
            api_key = "k"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.irc.sasl_password.as_deref(), Some("s3cret"));
        assert_eq!(cfg.irc.channels[0].default_role, Role::ReadOnly);
        assert_eq!(cfg.services[1].name(), "sonarr4k");
        assert_eq!(cfg.bot.prefix, "!");
    }

    #[test]
    fn env_overrides_beat_the_file() {
        let mut cfg = Config::parse(
            "[irc]\nnick = \"bot\"\n[web]\npassword = \"x\"\nlisten = \"127.0.0.1:1\"\n[bot]\ndata_dir = \"d\"\n",
        )
        .unwrap();
        unsafe {
            std::env::set_var("MEDIABOT_WEB_LISTEN", "0.0.0.0:8088");
            std::env::set_var("MEDIABOT_DATA_DIR", "/data");
        }
        cfg.apply_env_overrides().unwrap();
        assert_eq!(cfg.web.listen.to_string(), "0.0.0.0:8088");
        assert_eq!(cfg.bot.data_dir, std::path::PathBuf::from("/data"));
        unsafe { std::env::set_var("MEDIABOT_WEB_LISTEN", "nonsense") };
        assert!(cfg.apply_env_overrides().is_err());
        unsafe {
            std::env::remove_var("MEDIABOT_WEB_LISTEN");
            std::env::remove_var("MEDIABOT_DATA_DIR");
        }
    }

    #[test]
    fn rejects_duplicate_names() {
        let err = Config::parse(
            r#"
            [irc]
            nick = "bot"
            [web]
            enabled = false
            [[service]]
            kind = "radarr"
            url = "http://a"
            api_key = "k"
            [[service]]
            kind = "radarr"
            url = "http://b"
            api_key = "k"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    const BASE: &str = "[irc]\nnick = \"bot\"\n[web]\nenabled = false\n";

    #[test]
    fn disabled_service_may_reference_unset_vars() {
        // enabled = false comes after the secret on purpose: the whole block counts
        let cfg = Config::parse(&format!(
            "{BASE}\
             [[service]]\nkind = \"plex\"\nurl = \"${{MEDIABOT_UNSET_URL_X}}\"\napi_key = \"${{MEDIABOT_UNSET_KEY_X}}\"\nenabled = false\n\
             [[service]]\nkind = \"sonarr\"\nurl = \"http://s\"\napi_key = \"k\"\n"
        ))
        .unwrap();
        assert_eq!(cfg.services.len(), 2);
        assert!(!cfg.services[0].enabled && cfg.services[1].enabled);
        assert_eq!(cfg.services[0].api_key, "");
        let names: Vec<String> = cfg.enabled_services().map(|s| s.name()).collect();
        assert_eq!(names, ["sonarr"]);
    }

    #[test]
    fn enabled_service_with_unset_var_still_fails() {
        let err = Config::parse(&format!(
            "{BASE}[[service]]\nkind = \"plex\"\nurl = \"http://p\"\napi_key = \"${{MEDIABOT_UNSET_KEY_Y}}\"\n\
             [[service]]\nkind = \"emby\"\nurl = \"http://e\"\napi_key = \"k\"\nenabled = false\n"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("MEDIABOT_UNSET_KEY_Y"), "{err}");
        // a commented-out `enabled = false` doesn't disable anything
        let err = Config::parse(&format!(
            "{BASE}[[service]]\nkind = \"plex\"\nurl = \"http://p\"\napi_key = \"${{MEDIABOT_UNSET_KEY_Z}}\"\n# enabled = false\n"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("MEDIABOT_UNSET_KEY_Z"), "{err}");
    }

    #[test]
    fn media_source_and_timezone() {
        let cfg = Config::parse(BASE).unwrap();
        assert_eq!(cfg.bot.media_source, MediaSource::Auto);
        let cfg = Config::parse(&format!(
            "{BASE}[bot]\nmedia_source = \"direct\"\ntimezone = \"America/New_York\"\n"
        ))
        .unwrap();
        assert_eq!(cfg.bot.media_source, MediaSource::Direct);
        assert_eq!(cfg.timezone(), "America/New_York");
        assert!(Config::parse(&format!("{BASE}[bot]\nmedia_source = \"sometimes\"\n")).is_err());
        assert!(Config::parse(&format!("{BASE}[bot]\ntimezone = \"New York\"\n")).is_err());
        assert_eq!(
            Config::parse(&format!("{BASE}[[service]]\nkind = \"tracearr\"\nurl = \"https://t\"\napi_key = \"trr_pub_x\"\n"))
                .unwrap()
                .services[0]
                .kind,
            ServiceKind::Tracearr
        );
    }
}
