//! Logging: stdout (text or JSON), an in-memory ring buffer for the web panel, and
//! optional shipping to Loki.

use std::{collections::VecDeque, fmt::Write as _, sync::Arc};

use anyhow::{Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{
    EnvFilter, Layer, filter::LevelFilter, layer::SubscriberExt, registry::LookupSpan,
    util::SubscriberInitExt,
};

use crate::config::TelemetryConfig;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub level: String,
    pub target: String,
    pub message: String,
}

pub struct LogBuffer {
    inner: std::sync::Mutex<(u64, VecDeque<LogLine>)>,
    cap: usize,
}

impl LogBuffer {
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new((0, VecDeque::with_capacity(cap))),
            cap,
        })
    }

    fn push(&self, mut l: LogLine) {
        let mut g = self.inner.lock().unwrap();
        g.0 += 1;
        l.seq = g.0;
        if g.1.len() == self.cap {
            g.1.pop_front();
        }
        g.1.push_back(l);
    }

    pub fn since(&self, seq: u64) -> Vec<LogLine> {
        self.inner
            .lock()
            .unwrap()
            .1
            .iter()
            .filter(|l| l.seq > seq)
            .cloned()
            .collect()
    }
}

struct BufferLayer(Arc<LogBuffer>);

#[derive(Default)]
struct Fields {
    message: String,
    extra: String,
}

impl Visit for Fields {
    fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
        if f.name() == "message" {
            self.message.push_str(v);
        } else {
            let _ = write!(self.extra, " {}={v:?}", f.name());
        }
    }
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        if f.name() == "message" {
            let _ = write!(self.message, "{v:?}");
        } else {
            let _ = write!(self.extra, " {}={v:?}", f.name());
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for BufferLayer {
    fn on_event(&self, ev: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut f = Fields::default();
        ev.record(&mut f);
        let md = ev.metadata();
        self.0.push(LogLine {
            seq: 0,
            ts: Utc::now(),
            level: md.level().to_string(),
            target: md.target().to_string(),
            message: format!("{}{}", f.message, f.extra),
        });
    }
}

/// Install the global subscriber. Returns the Loki background task, if configured, which
/// the caller must spawn.
pub fn init(
    cfg: &TelemetryConfig,
    buffer: Arc<LogBuffer>,
) -> Result<Option<tracing_loki::BackgroundTask>> {
    let level = cfg.log_level.clone().unwrap_or_else(|| "info".into());
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&level));
    let stdout = if cfg.json_logs {
        tracing_subscriber::fmt::layer()
            .json()
            .with_filter(filter())
            .boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .with_filter(filter())
            .boxed()
    };
    let buf = BufferLayer(buffer).with_filter(LevelFilter::INFO).boxed();

    let (loki, task) = match &cfg.loki {
        None => (None, None),
        Some(l) => {
            // tracing-loki appends loki/api/v1/push itself.
            let base = l
                .url
                .trim_end_matches('/')
                .trim_end_matches("/loki/api/v1/push")
                .to_string()
                + "/";
            let url = url::Url::parse(&base).context("telemetry.loki.url")?;
            let mut b = tracing_loki::builder()
                .label("app", "mediabot")?
                .extra_field("pid", std::process::id().to_string())?;
            let host = std::fs::read_to_string("/etc/hostname")
                .map(|h| h.trim().to_string())
                .unwrap_or_default();
            if !host.is_empty() && !l.labels.contains_key("host") {
                b = b.label("host", host)?;
            }
            for (k, v) in &l.labels {
                b = b.label(k, v)?;
            }
            if let (Some(u), Some(p)) = (&l.username, &l.password) {
                let token = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
                b = b.http_header("Authorization", format!("Basic {token}"))?;
            }
            if let Some(t) = &l.tenant {
                b = b.http_header("X-Scope-OrgID", t)?;
            }
            let (layer, task) = b.build_url(url)?;
            (Some(layer.with_filter(filter()).boxed()), Some(task))
        }
    };

    tracing_subscriber::registry()
        .with(stdout)
        .with(buf)
        .with(loki)
        .try_init()?;
    Ok(task)
}
