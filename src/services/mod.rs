//! Backend API clients. One [`Service`] per configured `[[service]]`; the per-kind APIs live
//! in submodules as `impl Service` blocks.

pub mod arr;
pub mod media;
pub mod seerr;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Method, header};
use serde_json::Value;

use crate::{
    config::{ServiceConfig, ServiceKind},
    metrics::Metrics,
};

pub struct Service {
    pub name: String,
    pub kind: ServiceKind,
    base: String,
    key: String,
    http: reqwest::Client,
    metrics: Arc<Metrics>,
    /// Seerr's CSRF cookies + token, fetched the first time a write is refused (see [`Csrf`]).
    csrf: tokio::sync::Mutex<Option<Csrf>>,
}

/// Seerr with "CSRF Protection" on (Settings → Network) rejects every POST - API key or not - unless it
/// carries the `_csrf` cookie and the matching token in an `X-XSRF-TOKEN` header. Seerr hands both out as
/// cookies on any response, marked `Secure` in production, so a normal cookie jar would drop them over
/// plain http; they're read from `Set-Cookie` and sent back by hand instead.
#[derive(Clone, Debug, PartialEq)]
struct Csrf {
    /// Every cookie Seerr set, as a `Cookie:` header value.
    cookie: String,
    /// The decoded `XSRF-TOKEN` cookie.
    token: String,
}

impl Csrf {
    fn from_set_cookies<'a>(values: impl Iterator<Item = &'a str>) -> Option<Csrf> {
        let mut pairs = vec![];
        let mut token = None;
        for v in values {
            let Some((name, value)) = v.split(';').next().and_then(|p| p.trim().split_once('='))
            else {
                continue;
            };
            if name == "XSRF-TOKEN" {
                token = Some(
                    percent_encoding::percent_decode_str(value)
                        .decode_utf8_lossy()
                        .into_owned(),
                );
            }
            pairs.push(format!("{name}={value}"));
        }
        Some(Csrf {
            cookie: pairs.join("; "),
            token: token?,
        })
    }
}

fn is_csrf_rejection(status: reqwest::StatusCode, body: &[u8]) -> bool {
    status == reqwest::StatusCode::FORBIDDEN
        && String::from_utf8_lossy(body)
            .to_ascii_lowercase()
            .contains("csrf")
}

impl Service {
    pub fn new(cfg: &ServiceConfig, metrics: Arc<Metrics>) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json"),
        );
        let key = cfg.api_key.clone();
        match cfg.kind {
            ServiceKind::Plex => {
                headers.insert("X-Plex-Token", header::HeaderValue::from_str(&key)?);
                headers.insert(
                    "X-Plex-Client-Identifier",
                    header::HeaderValue::from_static("mediabot"),
                );
                headers.insert(
                    "X-Plex-Product",
                    header::HeaderValue::from_static("mediabot"),
                );
            }
            ServiceKind::Emby => {
                headers.insert("X-Emby-Token", header::HeaderValue::from_str(&key)?);
            }
            ServiceKind::Jellyfin => {
                let v = format!(
                    r#"MediaBrowser Client="mediabot", Device="mediabot", DeviceId="mediabot", Version="{}", Token="{key}""#,
                    env!("CARGO_PKG_VERSION")
                );
                headers.insert(header::AUTHORIZATION, header::HeaderValue::from_str(&v)?);
            }
            ServiceKind::Tautulli => {} // query parameter
            ServiceKind::Tofa => {
                // Admin > Settings > API > API keys ("Read only" is enough).
                headers.insert(
                    header::AUTHORIZATION,
                    header::HeaderValue::from_str(&format!("Bearer {key}"))?,
                );
            }
            ServiceKind::Seerr
            | ServiceKind::Sonarr
            | ServiceKind::Radarr
            | ServiceKind::Lidarr => {
                headers.insert("X-Api-Key", header::HeaderValue::from_str(&key)?);
            }
        }
        for v in headers.values_mut() {
            v.set_sensitive(true);
        }
        headers
            .get_mut(header::ACCEPT)
            .unwrap()
            .set_sensitive(false);
        let http = reqwest::Client::builder()
            .user_agent(concat!("mediabot/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .danger_accept_invalid_certs(!cfg.verify_tls)
            .build()?;
        Ok(Self {
            name: cfg.name(),
            kind: cfg.kind,
            base: cfg.url.trim_end_matches('/').to_string(),
            key,
            http,
            metrics,
            csrf: tokio::sync::Mutex::new(None),
        })
    }

    fn url(&self, path: &str, query: &[(&str, String)]) -> String {
        let mut u = format!("{}{}", self.base, path);
        let mut params: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        if self.kind == ServiceKind::Tautulli {
            params.push(("apikey", &self.key));
        }
        for (i, (k, v)) in params.iter().enumerate() {
            u.push(if i == 0 && !path.contains('?') {
                '?'
            } else {
                '&'
            });
            u.push_str(k);
            u.push('=');
            // Strict encoding (spaces as %20): Seerr rejects '+' and raw punctuation.
            u.extend(utf8_percent_encode(v, NON_ALPHANUMERIC));
        }
        u
    }

    /// One HTTP round trip; records latency. `csrf` adds Seerr's CSRF cookie + header.
    async fn send(
        &self,
        method: &Method,
        url: &str,
        body: Option<&Value>,
        csrf: Option<&Csrf>,
    ) -> Result<(reqwest::StatusCode, Vec<u8>)> {
        let mut rb = self.http.request(method.clone(), url);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        if let Some(c) = csrf {
            let mut cookie =
                header::HeaderValue::from_str(&c.cookie).context("Seerr CSRF cookie")?;
            cookie.set_sensitive(true);
            let mut token = header::HeaderValue::from_str(&c.token).context("Seerr CSRF token")?;
            token.set_sensitive(true);
            rb = rb
                .header(header::COOKIE, cookie)
                .header("X-XSRF-TOKEN", token);
        }
        let start = Instant::now();
        let res = rb.send().await;
        self.metrics
            .service_latency
            .with_label_values(&[&self.name, self.kind.as_str()])
            .observe(start.elapsed().as_secs_f64());
        let resp = res.map_err(|e| anyhow!("{}: {}", self.name, strip_url(&e)))?;
        let status = resp.status();
        let bytes = resp.bytes().await.context("reading body")?;
        Ok((status, bytes.to_vec()))
    }

    /// Get Seerr's CSRF cookies from any response (`/api/v1/status` needs no auth).
    async fn fetch_csrf(&self) -> Result<Csrf> {
        let resp = self
            .http
            .get(self.url("/api/v1/status", &[]))
            .send()
            .await
            .map_err(|e| anyhow!("{}: {}", self.name, strip_url(&e)))?;
        let values = resp.headers().get_all(header::SET_COOKIE);
        Csrf::from_set_cookies(values.iter().filter_map(|v| v.to_str().ok())).ok_or_else(|| {
            anyhow!(
                "{}: Seerr refused the request as a CSRF failure but sent no XSRF-TOKEN cookie",
                self.name
            )
        })
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let url = self.url(path, query);
        // only Seerr writes can be refused for CSRF; GETs never are
        let csrf_ok =
            self.kind == ServiceKind::Seerr && method != Method::GET && method != Method::HEAD;
        let outcome = async {
            let cached = if csrf_ok { self.csrf.lock().await.clone() } else { None };
            let (mut status, mut bytes) = self.send(&method, &url, body.as_ref(), cached.as_ref()).await?;
            if csrf_ok && is_csrf_rejection(status, &bytes) {
                // first write with CSRF Protection on, or the cached token went stale: fetch, retry once
                let fresh = self.fetch_csrf().await?;
                *self.csrf.lock().await = Some(fresh.clone());
                (status, bytes) = self.send(&method, &url, body.as_ref(), Some(&fresh)).await?;
                if is_csrf_rejection(status, &bytes) {
                    bail!(
                        "{} {method} {path}: Seerr still rejects the CSRF token (HTTP {status}); check \
                         Settings → Network → CSRF Protection",
                        self.name
                    );
                }
            }
            if !status.is_success() {
                let snippet =
                    String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).replace('\n', " ");
                bail!("{} {method} {path}: HTTP {status} {snippet}", self.name);
            }
            if bytes.is_empty() {
                return Ok(Value::Null);
            }
            serde_json::from_slice(&bytes)
                .with_context(|| format!("{} {path}: invalid JSON", self.name))
        }
        .await;
        let result = if outcome.is_ok() { "ok" } else { "error" };
        self.metrics
            .service_requests
            .with_label_values(&[&self.name, self.kind.as_str(), result])
            .inc();
        outcome
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.request(Method::GET, path, query, None).await
    }

    /// Version string; also proves the URL and API key work.
    pub async fn version(&self) -> Result<String> {
        let v = match self.kind {
            ServiceKind::Plex => {
                self.get("/identity", &[]).await?["MediaContainer"]["version"].clone()
            }
            ServiceKind::Emby | ServiceKind::Jellyfin => {
                self.get("/System/Info", &[]).await?["Version"].clone()
            }
            ServiceKind::Tofa => {
                let v = self.get("/api/v1/system/info", &[]).await?;
                // The schema is undocumented; take the most specific version field present.
                let ver = ["server_version", "version", "api_version"]
                    .iter()
                    .map(|k| s(&v, k))
                    .find(|x| !x.is_empty())
                    .unwrap_or_default();
                Value::String(ver)
            }
            ServiceKind::Tautulli => {
                let r = self
                    .get("/api/v2", &[("cmd", "get_tautulli_info".into())])
                    .await?;
                tautulli_data(&r)?["tautulli_version"].clone()
            }
            ServiceKind::Seerr => {
                // /status is unauthenticated; /auth/me validates the key.
                self.get("/api/v1/auth/me", &[]).await?;
                self.get("/api/v1/status", &[]).await?["version"].clone()
            }
            ServiceKind::Sonarr | ServiceKind::Radarr | ServiceKind::Lidarr => self
                .get(&format!("{}/system/status", self.arr_api()), &[])
                .await?["version"]
                .clone(),
        };
        Ok(v.as_str().unwrap_or("?").to_string())
    }
}

fn strip_url(e: &reqwest::Error) -> String {
    // Never leak query-string API keys (Tautulli) into logs or IRC.
    let s = e.without_url_ref().to_string();
    if e.is_timeout() {
        format!("timeout ({s})")
    } else if e.is_connect() {
        format!("connect failed ({s})")
    } else {
        s
    }
}

trait WithoutUrl {
    fn without_url_ref(&self) -> String;
}
impl WithoutUrl for reqwest::Error {
    fn without_url_ref(&self) -> String {
        let mut s = self.to_string();
        if let Some(u) = self.url() {
            s = s.replace(u.as_str(), "<url>");
        }
        s
    }
}

pub(crate) fn tautulli_data(v: &Value) -> Result<&Value> {
    let r = &v["response"];
    if r["result"].as_str() != Some("success") {
        bail!(
            "tautulli: {}",
            r["message"].as_str().unwrap_or("request failed")
        );
    }
    Ok(&r["data"])
}

/// String field (empty if missing).
pub(crate) fn s(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Numeric field that may be encoded as a string (Tautulli does this).
pub(crate) fn n(v: &Value, k: &str) -> Option<f64> {
    match &v[k] {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_encoding_and_tautulli_key() {
        let m = Arc::new(Metrics::new());
        let cfg = ServiceConfig {
            name: None,
            kind: ServiceKind::Tautulli,
            url: "http://t:8181/".into(),
            api_key: "abc".into(),
            verify_tls: true,
            timeout_secs: 1,
        };
        let s = Service::new(&cfg, m).unwrap();
        assert_eq!(
            s.url("/api/v2", &[("cmd", "get activity!".into())]),
            "http://t:8181/api/v2?cmd=get%20activity%21&apikey=abc"
        );
    }

    #[test]
    fn csrf_cookies_parsed() {
        let c = Csrf::from_set_cookies(
            [
                "_csrf=s3cr3t; Path=/; HttpOnly; Secure; SameSite=Strict",
                "XSRF-TOKEN=abc%3Ddef; Path=/; Secure; SameSite=Strict",
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(c.cookie, "_csrf=s3cr3t; XSRF-TOKEN=abc%3Ddef");
        assert_eq!(c.token, "abc=def");
        assert!(Csrf::from_set_cookies(["connect.sid=x; Path=/api"].into_iter()).is_none());
    }

    /// A fake Seerr: every response sets `_csrf` + `XSRF-TOKEN` (Secure, like production). With `csrf_on`,
    /// a POST without the matching cookie and X-XSRF-TOKEN header gets 403 "invalid csrf token".
    /// `secret` can be changed mid-test to simulate the server rotating its secret. Returns the base URL
    /// and a log of "METHOD path status" lines.
    async fn fake_seerr(
        csrf_on: bool,
        secret: Arc<std::sync::Mutex<String>>,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(std::sync::Mutex::new(vec![]));
        let log2 = log.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let (log, secret) = (log2.clone(), secret.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let mut n = 0;
                    while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        let r = sock.read(&mut buf[n..]).await.unwrap();
                        if r == 0 {
                            return;
                        }
                        n += r;
                    }
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let first = req.lines().next().unwrap_or("").to_string();
                    let (method, path) = {
                        let mut p = first.split_whitespace();
                        (
                            p.next().unwrap_or("").to_string(),
                            p.next().unwrap_or("").to_string(),
                        )
                    };
                    let hdr = |name: &str| {
                        req.lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                            })
                            .unwrap_or_default()
                    };
                    let secret = secret.lock().unwrap().clone();
                    let token = format!("tok-{secret}=");
                    let (status, body) = if method == "POST" && csrf_on {
                        let ok = hdr("cookie").contains(&format!("_csrf={secret}"))
                            && hdr("x-xsrf-token") == token;
                        if ok {
                            ("201 Created", r#"{"id":77,"status":2}"#)
                        } else {
                            ("403 Forbidden", r#"{"message":"invalid csrf token"}"#)
                        }
                    } else if method == "POST" {
                        ("201 Created", r#"{"id":77,"status":2}"#)
                    } else {
                        ("200 OK", r#"{"version":"2.7.0"}"#)
                    };
                    log.lock()
                        .unwrap()
                        .push(format!("{method} {path} {}", &status[..3]));
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                         Set-Cookie: _csrf={secret}; Path=/; HttpOnly; Secure; SameSite=Strict\r\n\
                         Set-Cookie: XSRF-TOKEN={}; Path=/; Secure; SameSite=Strict\r\n\
                         Connection: close\r\n\r\n{body}",
                        body.len(),
                        token.replace('=', "%3D"),
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), log)
    }

    fn seerr_service(url: &str) -> Service {
        let cfg = ServiceConfig {
            name: None,
            kind: ServiceKind::Seerr,
            url: url.into(),
            api_key: "k".into(),
            verify_tls: true,
            timeout_secs: 5,
        };
        Service::new(&cfg, Arc::new(Metrics::new())).unwrap()
    }

    #[tokio::test]
    async fn seerr_csrf_fetched_retried_and_cached() {
        let secret = Arc::new(std::sync::Mutex::new("one".to_string()));
        let (url, log) = fake_seerr(true, secret.clone()).await;
        let s = seerr_service(&url);

        let r = s.seerr_request("movie", 603, &[]).await.unwrap();
        assert_eq!(r["id"], 77);
        assert_eq!(
            *log.lock().unwrap(),
            [
                "POST /api/v1/request 403",
                "GET /api/v1/status 200",
                "POST /api/v1/request 201"
            ]
        );

        // cached: one request
        log.lock().unwrap().clear();
        s.seerr_moderate(5, "approve").await.unwrap();
        assert_eq!(*log.lock().unwrap(), ["POST /api/v1/request/5/approve 201"]);

        // server rotated its secret: stale token refused, refreshed, retried once
        *secret.lock().unwrap() = "two".into();
        log.lock().unwrap().clear();
        s.seerr_moderate(6, "decline").await.unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            [
                "POST /api/v1/request/6/decline 403",
                "GET /api/v1/status 200",
                "POST /api/v1/request/6/decline 201"
            ]
        );
    }

    #[tokio::test]
    async fn seerr_without_csrf_needs_no_extra_request() {
        let (url, log) = fake_seerr(false, Arc::new(std::sync::Mutex::new("x".into()))).await;
        let s = seerr_service(&url);
        s.seerr_request("tv", 95396, &[1, 2]).await.unwrap();
        assert_eq!(*log.lock().unwrap(), ["POST /api/v1/request 201"]);
    }
}
