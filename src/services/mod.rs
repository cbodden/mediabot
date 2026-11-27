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

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let url = self.url(path, query);
        let mut rb = self.http.request(method.clone(), &url);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let start = Instant::now();
        let res = rb.send().await;
        self.metrics
            .service_latency
            .with_label_values(&[&self.name, self.kind.as_str()])
            .observe(start.elapsed().as_secs_f64());
        let outcome = async {
            let resp = res.map_err(|e| anyhow!("{}: {}", self.name, strip_url(&e)))?;
            let status = resp.status();
            let bytes = resp.bytes().await.context("reading body")?;
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
}
