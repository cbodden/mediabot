//! Background poller: refreshes Prometheus gauges and per-service health for the panel.
//!
//! Tracearr services are polled first. When the direct media servers should sit on standby
//! (`media_source = "tracearr"`, or `"auto"` while a Tracearr is up) they are not polled that
//! cycle: they're marked `standby` and their stream / library / up series are removed, so
//! dashboards never show stale or double-counted streams.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::{
    app::App,
    config::ServiceKind,
    metrics::Metrics,
    services::{Service, media::Library, tracearr::Backend},
};

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub kind: ServiceKind,
    pub up: bool,
    /// A direct media server Tracearr is standing in for: not polled, not asked by `np`/`libs`.
    pub standby: bool,
    pub version: Option<String>,
    pub error: Option<String>,
    pub last_poll: Option<DateTime<Utc>>,
    pub latency_ms: Option<u64>,
    pub summary: Vec<String>,
    /// Tracearr only: the media servers it monitors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub backends: Vec<Backend>,
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
            standby: false,
            version: None,
            error: None,
            last_poll: None,
            latency_ms: None,
            summary: vec![],
            backends: vec![],
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

async fn poll_set(app: &Arc<App>, svcs: Vec<Arc<Service>>, slow: bool) {
    let mut set = JoinSet::new();
    for svc in svcs {
        let app = app.clone();
        set.spawn(async move {
            let prev = app
                .service_status
                .read()
                .await
                .get(&svc.name)
                .cloned()
                .unwrap_or_else(|| ServiceStatus::new(&svc));
            let st = poll_one(&app, &svc, prev, slow).await;
            app.service_status
                .write()
                .await
                .insert(svc.name.clone(), st);
        });
    }
    while set.join_next().await.is_some() {}
}

pub async fn poll_all(app: &Arc<App>, slow: bool) {
    let (tracearr, others): (Vec<_>, Vec<_>) = app
        .services
        .iter()
        .cloned()
        .partition(|s| s.kind == ServiceKind::Tracearr);
    poll_set(app, tracearr.clone(), slow).await;

    let tracearr_up = {
        let st = app.service_status.read().await;
        tracearr
            .iter()
            .any(|s| st.get(&s.name).is_some_and(|x| x.up))
    };
    let standby = app.direct_on_standby(tracearr_up);
    let (sleeping, active): (Vec<_>, Vec<_>) = others
        .into_iter()
        .partition(|s| standby && s.kind.is_direct_media());
    for svc in &sleeping {
        let mut st = app.service_status.write().await;
        let entry = st
            .entry(svc.name.clone())
            .or_insert_with(|| ServiceStatus::new(svc));
        if !entry.standby {
            info!(service = %svc.name, "standby: Tracearr answers for it");
        }
        put_on_standby(&app.metrics, svc, entry);
    }
    poll_set(app, active, slow).await;
}

/// Mark a direct server as standing by and drop its series so nothing stale or doubled remains.
fn put_on_standby(m: &Metrics, svc: &Service, st: &mut ServiceStatus) {
    let (name, kind) = (svc.name.as_str(), svc.kind.as_str());
    st.standby = true;
    st.error = None;
    st.summary = vec!["standby: Tracearr answers for it".into()];
    for d in ["transcode", "direct"] {
        let _ = m.streams.remove_label_values(&[name, kind, d]);
    }
    let _ = m.stream_bandwidth.remove_label_values(&[name, kind]);
    let _ = m.service_up.remove_label_values(&[name, kind]);
    for (lib, t) in std::mem::take(&mut st.libs) {
        let _ = m.library_items.remove_label_values(&[name, kind, &lib, &t]);
    }
    m.service_standby.with_label_values(&[name, kind]).set(1);
}

async fn poll_one(app: &App, svc: &Service, mut st: ServiceStatus, slow: bool) -> ServiceStatus {
    let started = Instant::now();
    let was_up = st.up;
    // coming back from standby: refresh everything, including libraries
    let slow = slow || st.standby;
    if st.standby {
        info!(service = %svc.name, "leaving standby");
        st.standby = false;
        st.summary.clear();
    }
    if svc.kind.is_direct_media() {
        app.metrics
            .service_standby
            .with_label_values(&[&svc.name, svc.kind.as_str()])
            .set(0);
    }
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
            if !was_up && st.error.is_some() {
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

/// Export library counts, removing label sets that disappeared since the last export.
fn export_libs(m: &Metrics, svc: &Service, libs: &[Library], st: &mut ServiceStatus) {
    let (name, kind) = (svc.name.as_str(), svc.kind.as_str());
    let mut seen = vec![];
    for l in libs {
        if let Some(c) = l.count {
            m.library_items
                .with_label_values(&[name, kind, &l.name, &l.kind])
                .set(c as i64);
            seen.push((l.name.clone(), l.kind.clone()));
        }
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
}

fn export_streams(m: &Metrics, svc: &Service, act: &crate::services::media::Activity) -> String {
    let (name, kind) = (svc.name.as_str(), svc.kind.as_str());
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
    format!("{total} stream(s), {tc} transcoding")
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
        ServiceKind::Tracearr => {
            let (_, backends) = svc.tracearr_servers().await?;
            for b in &backends {
                m.tracearr_server_up
                    .with_label_values(&[name, &b.name, &b.kind])
                    .set(b.up as i64);
                m.tracearr_server_streams
                    .with_label_values(&[name, &b.name])
                    .set(b.active_streams);
            }
            let up = backends.iter().filter(|b| b.up).count();
            st.backends = backends;
            summary.push(export_streams(m, svc, &svc.activity().await?));
            summary.push(format!("{up}/{} servers up", st.backends.len()));
            let today = svc.tracearr_today(&app.config.timezone()).await?;
            for (stat, v) in [
                ("plays", today.plays as f64),
                ("sessions", today.sessions as f64),
                ("watch_hours", today.watch_hours),
                ("active_users", today.active_users as f64),
                ("alerts", today.alerts as f64),
            ] {
                m.tracearr_today.with_label_values(&[name, stat]).set(v);
            }
            summary.push(format!(
                "today {} plays, {:.1} h",
                today.plays, today.watch_hours
            ));
            if slow {
                let libs = svc.libraries().await?;
                export_libs(m, svc, &libs, st);
            }
        }
        k if k.is_direct_media() => {
            summary.push(export_streams(m, svc, &svc.activity().await?));
            if slow {
                let libs = svc.libraries().await?;
                export_libs(m, svc, &libs, st);
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
