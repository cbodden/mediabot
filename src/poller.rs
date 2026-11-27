//! Background poller: refreshes Prometheus gauges and per-service health for the panel.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::{app::App, config::ServiceKind, services::Service};

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub kind: ServiceKind,
    pub up: bool,
    pub version: Option<String>,
    pub error: Option<String>,
    pub last_poll: Option<DateTime<Utc>>,
    pub latency_ms: Option<u64>,
    pub summary: Vec<String>,
    /// Library label sets last exported, so stale ones can be removed.
    #[serde(skip)]
    libs: Vec<(String, String)>,
}

impl ServiceStatus {
    pub fn new(s: &Service) -> Self {
        Self {
            name: s.name.clone(),
            kind: s.kind,
            up: false,
            version: None,
            error: None,
            last_poll: None,
            latency_ms: None,
            summary: vec![],
            libs: vec![],
        }
    }
}

pub async fn run(app: Arc<App>) {
    {
        let mut st = app.service_status.write().await;
        for s in &app.services {
            st.insert(s.name.clone(), ServiceStatus::new(s));
        }
    }
    let every = app.config.bot.slow_poll_every.max(1);
    let mut tick = tokio::time::interval(Duration::from_secs(
        app.config.bot.poll_interval_secs.max(5),
    ));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut n: u64 = 0;
    loop {
        tick.tick().await;
        poll_all(&app, n.is_multiple_of(every)).await;
        app.refresh_user_metrics().await;
        n += 1;
    }
}

pub async fn poll_all(app: &Arc<App>, slow: bool) {
    let mut set = JoinSet::new();
    for svc in app.services.clone() {
        let app = app.clone();
        set.spawn(async move {
            let prev = app
                .service_status
                .read()
                .await
                .get(&svc.name)
                .cloned()
                .unwrap_or_else(|| ServiceStatus::new(&svc));
            // Re-fetch the version on slow polls and whenever we were down.
            let st = poll_one(&app, &svc, prev, slow).await;
            app.service_status
                .write()
                .await
                .insert(svc.name.clone(), st);
        });
    }
    while set.join_next().await.is_some() {}
}

async fn poll_one(app: &App, svc: &Service, mut st: ServiceStatus, slow: bool) -> ServiceStatus {
    let started = Instant::now();
    let was_up = st.up;
    let res: Result<()> = async {
        if slow || !st.up || st.version.is_none() {
            st.version = Some(svc.version().await?);
        }
        st.summary = collect(app, svc, &mut st, slow).await?;
        Ok(())
    }
    .await;
    st.latency_ms = Some(started.elapsed().as_millis() as u64);
    st.last_poll = Some(Utc::now());
    match res {
        Ok(()) => {
            if !was_up && st.last_poll.is_some() && st.error.is_some() {
                info!(service = %svc.name, "service recovered");
            }
            st.up = true;
            st.error = None;
        }
        Err(e) => {
            if was_up || st.error.is_none() {
                warn!(service = %svc.name, "poll failed: {e:#}");
            }
            st.up = false;
            st.error = Some(format!("{e:#}"));
        }
    }
    app.metrics
        .service_up
        .with_label_values(&[&svc.name, svc.kind.as_str()])
        .set(st.up as i64);
    st
}

async fn collect(
    app: &App,
    svc: &Service,
    st: &mut ServiceStatus,
    slow: bool,
) -> Result<Vec<String>> {
    let m = &app.metrics;
    let (name, kind) = (svc.name.as_str(), svc.kind.as_str());
    let mut summary = vec![];
    match svc.kind {
        k if k.is_media_server() => {
            let act = svc.activity().await?;
            let tc = act.streams.iter().filter(|s| s.transcode).count() as i64;
            let total = act.streams.len() as i64;
            m.streams
                .with_label_values(&[name, kind, "transcode"])
                .set(tc);
            m.streams
                .with_label_values(&[name, kind, "direct"])
                .set(total - tc);
            if let Some(bw) = act.bandwidth_kbps {
                m.stream_bandwidth.with_label_values(&[name, kind]).set(bw);
            }
            summary.push(format!("{total} stream(s), {tc} transcoding"));
            if slow {
                let libs = svc.libraries().await?;
                let mut seen = vec![];
                for l in &libs {
                    m.library_items
                        .with_label_values(&[name, kind, &l.name, &l.kind])
                        .set(l.count as i64);
                    seen.push((l.name.clone(), l.kind.clone()));
                    if let Some(c) = l.children {
                        let ck = format!("{}_children", l.kind);
                        m.library_items
                            .with_label_values(&[name, kind, &l.name, &ck])
                            .set(c as i64);
                        seen.push((l.name.clone(), ck));
                    }
                }
                for (lib, t) in st.libs.iter().filter(|x| !seen.contains(x)) {
                    let _ = m.library_items.remove_label_values(&[name, kind, lib, t]);
                }
                st.libs = seen;
                summary.push(format!("{} libraries", libs.len()));
            } else if !st.summary.is_empty() {
                summary.extend(st.summary.iter().skip(1).cloned());
            }
        }
        ServiceKind::Seerr => {
            let counts = svc.seerr_counts().await?;
            for (k, v) in &counts {
                m.requests.with_label_values(&[name, k]).set(*v);
            }
            let get = |k: &str| counts.iter().find(|(n, _)| n == k).map_or(0, |(_, v)| *v);
            summary.push(format!(
                "{} pending, {} processing, {} total",
                get("pending"),
                get("processing"),
                get("total")
            ));
        }
        k if k.is_arr() => {
            let (queued, _) = svc.arr_queue(1).await?;
            m.arr_queue.with_label_values(&[name, kind]).set(queued);
            let health = svc.arr_health().await?;
            for level in ["error", "warning", "notice"] {
                let c = health.iter().filter(|h| h.level == level).count() as i64;
                m.arr_health.with_label_values(&[name, kind, level]).set(c);
            }
            summary.push(format!("queue {queued}"));
            if !health.is_empty() {
                summary.push(format!("{} health issues", health.len()));
            }
            if slow {
                let (items, missing) = svc.arr_totals().await?;
                m.arr_items.with_label_values(&[name, kind]).set(items);
                m.arr_missing.with_label_values(&[name, kind]).set(missing);
                summary.push(format!("{items} items, {missing} missing"));
            } else if let Some(prev) = st.summary.iter().find(|s| s.contains(" items, ")) {
                summary.push(prev.clone());
            }
        }
        _ => {}
    }
    Ok(summary)
}
