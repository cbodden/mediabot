//! Prometheus metrics. Everything is prefixed `mediabot_`.

use prometheus::{
    Encoder, GaugeVec, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge,
    IntGaugeVec, Opts, Registry, TextEncoder,
};

pub struct Metrics {
    pub registry: Registry,
    // bot / irc
    pub irc_connected: IntGauge,
    pub irc_reconnects: IntCounter,
    pub irc_lines: IntCounterVec,
    pub irc_channels: IntGauge,
    pub commands: IntCounterVec,
    pub command_duration: HistogramVec,
    pub users: IntGaugeVec,
    pub build_info: IntGaugeVec,
    pub start_time: IntGauge,
    // backends
    pub service_up: IntGaugeVec,
    pub service_requests: IntCounterVec,
    pub service_latency: HistogramVec,
    // media servers
    pub streams: IntGaugeVec,
    pub stream_bandwidth: GaugeVec,
    pub library_items: IntGaugeVec,
    // seerr
    pub requests: IntGaugeVec,
    // arr
    pub arr_queue: IntGaugeVec,
    pub arr_items: IntGaugeVec,
    pub arr_missing: IntGaugeVec,
    pub arr_health: IntGaugeVec,
}

macro_rules! reg {
    ($r:expr, $m:expr) => {{
        let m = $m.expect("metric");
        $r.register(Box::new(m.clone())).expect("register");
        m
    }};
}

impl Metrics {
    pub fn new() -> Self {
        let r = Registry::new();
        let o = |n: &str, h: &str| Opts::new(format!("mediabot_{n}"), h.to_string());
        let lat = vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];
        let m = Self {
            irc_connected: reg!(
                r,
                IntGauge::with_opts(o("irc_connected", "1 when registered on IRC"))
            ),
            irc_reconnects: reg!(
                r,
                IntCounter::with_opts(o("irc_reconnects_total", "IRC (re)connection attempts"))
            ),
            irc_lines: reg!(
                r,
                IntCounterVec::new(o("irc_lines_total", "IRC protocol lines"), &["direction"])
            ),
            irc_channels: reg!(
                r,
                IntGauge::with_opts(o("irc_channels", "Channels currently joined"))
            ),
            commands: reg!(
                r,
                IntCounterVec::new(
                    o("commands_total", "Bot commands handled"),
                    &["command", "result"]
                )
            ),
            command_duration: reg!(
                r,
                HistogramVec::new(
                    HistogramOpts::new(
                        "mediabot_command_duration_seconds",
                        "Command handling time"
                    )
                    .buckets(lat.clone()),
                    &["command"]
                )
            ),
            users: reg!(
                r,
                IntGaugeVec::new(o("acl_users", "ACL users by role"), &["role"])
            ),
            build_info: reg!(
                r,
                IntGaugeVec::new(o("build_info", "Build information"), &["version"])
            ),
            start_time: reg!(
                r,
                IntGauge::with_opts(o("start_time_seconds", "Process start time (unix)"))
            ),
            service_up: reg!(
                r,
                IntGaugeVec::new(
                    o("service_up", "1 if the last poll succeeded"),
                    &["service", "kind"]
                )
            ),
            service_requests: reg!(
                r,
                IntCounterVec::new(
                    o("service_requests_total", "HTTP requests to backends"),
                    &["service", "kind", "result"]
                )
            ),
            service_latency: reg!(
                r,
                HistogramVec::new(
                    HistogramOpts::new(
                        "mediabot_service_request_duration_seconds",
                        "Backend request latency"
                    )
                    .buckets(lat),
                    &["service", "kind"]
                )
            ),
            streams: reg!(
                r,
                IntGaugeVec::new(
                    o("streams", "Active streams"),
                    &["service", "kind", "decision"]
                )
            ),
            stream_bandwidth: reg!(
                r,
                GaugeVec::new(
                    o("stream_bandwidth_kbps", "Total streaming bandwidth (kbps)"),
                    &["service", "kind"]
                )
            ),
            library_items: reg!(
                r,
                IntGaugeVec::new(
                    o("library_items", "Items in media libraries"),
                    &["service", "kind", "library", "type"]
                )
            ),
            requests: reg!(
                r,
                IntGaugeVec::new(
                    o("seerr_requests", "Seerr requests by status"),
                    &["service", "status"]
                )
            ),
            arr_queue: reg!(
                r,
                IntGaugeVec::new(
                    o("arr_queue", "Items in the download queue"),
                    &["service", "kind"]
                )
            ),
            arr_items: reg!(
                r,
                IntGaugeVec::new(
                    o("arr_items", "Series/movies/artists managed"),
                    &["service", "kind"]
                )
            ),
            arr_missing: reg!(
                r,
                IntGaugeVec::new(
                    o("arr_missing", "Monitored items missing files"),
                    &["service", "kind"]
                )
            ),
            arr_health: reg!(
                r,
                IntGaugeVec::new(
                    o("arr_health_issues", "Health check issues"),
                    &["service", "kind", "type"]
                )
            ),
            registry: r,
        };
        m.build_info
            .with_label_values(&[env!("CARGO_PKG_VERSION")])
            .set(1);
        m.start_time.set(chrono::Utc::now().timestamp());
        m
    }

    pub fn render(&self) -> String {
        let mut buf = vec![];
        TextEncoder::new()
            .encode(&self.registry.gather(), &mut buf)
            .expect("encode");
        String::from_utf8(buf).unwrap_or_default()
    }
}
