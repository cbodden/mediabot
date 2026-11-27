//! Tracearr (<https://github.com/connorgallopo/Tracearr>): one monitor for Plex, Jellyfin, Emby and
//! Tofa. Public API v2 (`/api/v2/public/...`, snake_case) plus v1's `stats/today`, which v2 lacks.
//! Auth is `Authorization: Bearer trr_pub_...` (Settings → API).
//!
//! The parsers are plain functions over JSON so they can be tested against fixtures shaped like a
//! real instance's responses (v3.2.1).

use std::collections::HashMap;

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

use super::{
    Service,
    media::{Activity, Library, Stream, episode_title, first_nonempty, pct},
    n, s,
};
use crate::config::ServiceKind;

/// A media server as Tracearr sees it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Backend {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub up: bool,
    /// "up", "down" or "unknown".
    pub status: String,
    pub version: Option<String>,
    pub active_streams: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Today {
    pub plays: i64,
    pub sessions: i64,
    pub watch_hours: f64,
    pub active_users: i64,
    pub active_streams: i64,
    pub alerts: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TopItem {
    pub title: String,
    pub year: Option<i64>,
    pub plays: i64,
    pub watch_hours: f64,
    pub viewers: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Recent {
    pub title: String,
    pub year: Option<i64>,
    pub media_type: String,
    /// "2026-11-27 16:48" (UTC, from `added_at`).
    pub added: String,
    pub server: String,
}

fn data(v: &Value) -> &[Value] {
    v.get("data")
        .and_then(Value::as_array)
        .or_else(|| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn opt_i(v: &Value, k: &str) -> Option<i64> {
    n(v, k).map(|x| x as i64).filter(|x| *x > 0)
}

/// `GET /api/v2/public/servers` → (Tracearr version, backends).
pub fn parse_servers(v: &Value) -> (String, Vec<Backend>) {
    let backends = data(v)
        .iter()
        .map(|b| {
            let status = s(b, "status");
            Backend {
                id: s(b, "server_id"),
                name: first_nonempty(&[s(b, "server_name"), s(b, "server_id")]),
                kind: s(b, "server_type"),
                up: status == "up",
                status: if status.is_empty() {
                    "unknown".into()
                } else {
                    status
                },
                version: Some(s(b, "version")).filter(|x| !x.is_empty()),
                active_streams: n(b, "active_streams").unwrap_or(0.0) as i64,
            }
        })
        .collect();
    (s(v, "tracearr_version"), backends)
}

/// `GET /api/v2/public/streams` → the same shape the direct servers produce, one stream per
/// session, each labelled with its server. Bandwidth is the sum of the streams' kbps
/// (`summary.total_bitrate` is a display string such as "12.3 Mbps" or "—").
pub fn parse_streams(v: &Value) -> Activity {
    let mut bw = 0.0;
    let streams = data(v)
        .iter()
        .map(|x| {
            bw += n(x, "bitrate").unwrap_or(0.0);
            let title = match s(x, "media_type").as_str() {
                "episode" => episode_title(
                    &first_nonempty(&[s(x, "show_title"), s(x, "media_title")]),
                    n(x, "season_number"),
                    n(x, "episode_number"),
                    &s(x, "media_title"),
                ),
                "track" => format!(
                    "{} - {}",
                    first_nonempty(&[s(x, "artist_name"), s(x, "album_name")]),
                    s(x, "media_title")
                ),
                _ => match opt_i(x, "year") {
                    Some(y) => format!("{} ({y})", s(x, "media_title")),
                    None => s(x, "media_title"),
                },
            };
            let quality = Some(s(x, "resolution"))
                .filter(|r| !r.is_empty())
                .or_else(|| opt_i(x, "source_video_height").map(|h| format!("{h}p")));
            Stream {
                user: s(x, "username"),
                title,
                paused: s(x, "state") == "paused",
                progress: pct(n(x, "progress_ms"), n(x, "duration_ms")),
                transcode: x["is_transcode"].as_bool().unwrap_or(false)
                    || s(x, "video_decision") == "transcode",
                player: first_nonempty(&[
                    s(x, "device"),
                    s(x, "player"),
                    s(x, "product"),
                    s(x, "platform"),
                ]),
                quality,
                server: Some(s(x, "server_name")).filter(|n| !n.is_empty()),
            }
        })
        .collect();
    Activity {
        streams,
        bandwidth_kbps: Some(bw),
    }
}

/// `GET /api/v2/public/libraries` has counts but no library names, so libraries are added up per
/// server and type: movies, shows (with episodes), tracks, and anything else under "other".
pub fn parse_libraries(v: &Value, names: &HashMap<String, String>) -> Vec<Library> {
    // server name -> (movies, shows, episodes, tracks, other)
    let mut per: Vec<(String, [u64; 5])> = vec![];
    for l in data(v) {
        let id = s(l, "server_id");
        let name = names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| first_nonempty(&[s(l, "server_type"), id]));
        let c = |k| n(l, k).unwrap_or(0.0).max(0.0) as u64;
        let (movies, shows, eps, tracks) = (
            c("movie_count"),
            c("show_count"),
            c("episode_count"),
            c("track_count"),
        );
        let typed = movies + shows + tracks;
        let other = if typed == 0 { c("item_count") } else { 0 };
        let slot = match per.iter().position(|(n, _)| *n == name) {
            Some(i) => i,
            None => {
                per.push((name, [0; 5]));
                per.len() - 1
            }
        };
        let t = &mut per[slot].1;
        for (i, x) in [movies, shows, eps, tracks, other].into_iter().enumerate() {
            t[i] += x;
        }
    }
    let mut out = vec![];
    for (server, [movies, shows, eps, tracks, other]) in per {
        let mut push = |kind: &str, count: u64, children: Option<u64>| {
            if count > 0 {
                out.push(Library {
                    name: server.clone(),
                    kind: kind.into(),
                    count: Some(count),
                    children,
                    server: Some(server.clone()),
                });
            }
        };
        push("movie", movies, None);
        push("show", shows, Some(eps).filter(|e| *e > 0));
        push("track", tracks, None);
        push("other", other, None);
    }
    out
}

/// `GET /api/v1/public/stats/today`.
pub fn parse_today(v: &Value) -> Today {
    let i = |k| n(v, k).unwrap_or(0.0) as i64;
    Today {
        plays: i("todayPlays"),
        sessions: i("todaySessions"),
        watch_hours: n(v, "watchTimeHours").unwrap_or(0.0),
        active_users: i("activeUsersToday"),
        active_streams: i("activeStreams"),
        alerts: i("alertsLast24h"),
    }
}

/// `GET /api/v2/public/top-media`.
pub fn parse_top(v: &Value) -> Vec<TopItem> {
    data(v)
        .iter()
        .map(|t| TopItem {
            title: s(t, "title"),
            year: opt_i(t, "year"),
            plays: n(t, "plays").unwrap_or(0.0) as i64,
            watch_hours: n(t, "watch_time_ms").unwrap_or(0.0) / 3_600_000.0,
            viewers: n(t, "unique_users").unwrap_or(0.0) as i64,
        })
        .collect()
}

/// `GET /api/v2/public/recently-added`.
pub fn parse_recent(v: &Value, names: &HashMap<String, String>) -> Vec<Recent> {
    data(v)
        .iter()
        .filter(|r| r["removed_at"].is_null())
        .map(|r| {
            let id = s(r, "server_id");
            let added = s(r, "added_at");
            Recent {
                title: s(r, "title"),
                year: opt_i(r, "year"),
                media_type: s(r, "media_type"),
                added: if added.len() >= 16 {
                    added[..16].replace('T', " ")
                } else {
                    added
                },
                server: names
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| first_nonempty(&[s(r, "server_type"), id])),
            }
        })
        .collect()
}

impl Service {
    fn ensure_tracearr(&self) -> Result<()> {
        if self.kind != ServiceKind::Tracearr {
            bail!("{} is not a tracearr service", self.name);
        }
        Ok(())
    }

    pub async fn tracearr_servers(&self) -> Result<(String, Vec<Backend>)> {
        self.ensure_tracearr()?;
        Ok(parse_servers(
            &self.get("/api/v2/public/servers", &[]).await?,
        ))
    }

    async fn tracearr_names(&self) -> HashMap<String, String> {
        match self.tracearr_servers().await {
            Ok((_, b)) => b.into_iter().map(|b| (b.id, b.name)).collect(),
            Err(_) => HashMap::new(),
        }
    }

    pub(crate) async fn tracearr_activity(&self) -> Result<Activity> {
        self.ensure_tracearr()?;
        Ok(parse_streams(
            &self
                .get("/api/v2/public/streams", &[("summary", "true".into())])
                .await?,
        ))
    }

    pub(crate) async fn tracearr_libraries(&self) -> Result<Vec<Library>> {
        self.ensure_tracearr()?;
        let names = self.tracearr_names().await;
        Ok(parse_libraries(
            &self.get("/api/v2/public/libraries", &[]).await?,
            &names,
        ))
    }

    pub async fn tracearr_today(&self, timezone: &str) -> Result<Today> {
        self.ensure_tracearr()?;
        Ok(parse_today(
            &self
                .get(
                    "/api/v1/public/stats/today",
                    &[("timezone", timezone.into())],
                )
                .await?,
        ))
    }

    /// `media_type` is "movie" or "show"; `sort` is "plays", "watch_time" or "viewers".
    pub async fn tracearr_top(
        &self,
        media_type: &str,
        days: u32,
        sort: &str,
        limit: usize,
    ) -> Result<Vec<TopItem>> {
        self.ensure_tracearr()?;
        let v = self
            .get(
                "/api/v2/public/top-media",
                &[
                    ("media_type", media_type.into()),
                    ("days", days.to_string()),
                    ("sort", sort.into()),
                    ("pageSize", limit.to_string()),
                ],
            )
            .await?;
        Ok(parse_top(&v).into_iter().take(limit).collect())
    }

    /// `media_type`: movie, episode, season, show, artist, album, track or photo.
    pub async fn tracearr_recent(
        &self,
        media_type: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Recent>> {
        self.ensure_tracearr()?;
        let names = self.tracearr_names().await;
        let mut q = vec![("pageSize", limit.to_string())];
        if let Some(t) = media_type {
            q.push(("media_type", t.into()));
        }
        let v = self.get("/api/v2/public/recently-added", &q).await?;
        Ok(parse_recent(&v, &names).into_iter().take(limit).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names() -> HashMap<String, String> {
        [("p", "Plex"), ("e", "Emby")]
            .into_iter()
            .map(|(a, b)| (a.into(), b.into()))
            .collect()
    }

    #[test]
    fn servers() {
        let (ver, b) = parse_servers(&json!({
            "data": [
                {"server_id": "e", "server_name": "Emby", "status": "up", "reason": null, "server_type": "emby",
                 "historical": false, "active_streams": 0, "version": "4.10.2.0"},
                {"server_id": "j", "server_name": "Jellyfin", "status": "down", "server_type": "jellyfin",
                 "active_streams": 0, "version": "12.4.0"}
            ],
            "tracearr_version": "v3.2.1"
        }));
        assert_eq!(ver, "v3.2.1");
        assert_eq!(b.len(), 2);
        assert!(b[0].up && !b[1].up);
        assert_eq!(b[1].status, "down");
        assert_eq!(b[0].version.as_deref(), Some("4.10.2.0"));
    }

    #[test]
    fn idle_streams() {
        // what an idle instance really returns: no data key, a display-string bitrate
        let a =
            parse_streams(&json!({"summary": {"total": 0, "total_bitrate": "—", "by_server": []}}));
        assert!(a.streams.is_empty());
        assert_eq!(a.bandwidth_kbps, Some(0.0));
    }

    #[test]
    fn streams() {
        let a = parse_streams(&json!({
            "data": [
                {"server_name": "Plex", "username": "alice", "media_type": "episode", "media_title": "Pilot",
                 "show_title": "Severance", "season_number": 1, "episode_number": 1, "state": "playing",
                 "progress_ms": 1500, "duration_ms": 3000, "is_transcode": true, "video_decision": "transcode",
                 "bitrate": 8000, "resolution": "1080p", "device": "Living Room"},
                {"server_name": "Emby", "username": "bob", "media_type": "track", "media_title": "Song 2",
                 "artist_name": "Blur", "state": "paused", "progress_ms": 10, "duration_ms": 100,
                 "is_transcode": false, "video_decision": null, "bitrate": 320, "product": "Emby Web"},
                {"server_name": "Plex", "username": "carol", "media_type": "movie", "media_title": "Dune",
                 "year": 2021, "state": "playing", "is_transcode": false, "source_video_height": 2160,
                 "player": "Shield"}
            ],
            "summary": {"total": 3, "total_bitrate": "8.3 Mbps"}
        }));
        assert_eq!(a.bandwidth_kbps, Some(8320.0));
        let s0 = &a.streams[0];
        assert_eq!(s0.title, "Severance S01E01 - Pilot");
        assert!(s0.transcode && !s0.paused);
        assert_eq!(s0.progress, Some(50.0));
        assert_eq!(s0.quality.as_deref(), Some("1080p"));
        assert_eq!(s0.server.as_deref(), Some("Plex"));
        assert_eq!(a.streams[1].title, "Blur - Song 2");
        assert!(a.streams[1].paused && !a.streams[1].transcode);
        assert_eq!(a.streams[1].player, "Emby Web");
        assert_eq!(a.streams[2].title, "Dune (2021)");
        assert_eq!(a.streams[2].quality.as_deref(), Some("2160p"));
        assert_eq!(a.streams[2].progress, None);
    }

    #[test]
    fn libraries_summed_per_server() {
        let libs = parse_libraries(
            &json!({"data": [
                {"server_id": "e", "server_type": "emby", "library_id": "1", "item_count": 179644,
                 "movie_count": 0, "episode_count": 0, "show_count": 0, "track_count": 177116},
                {"server_id": "p", "server_type": "plex", "library_id": "2", "item_count": 600,
                 "movie_count": 500, "episode_count": 0, "show_count": 0, "track_count": 0},
                {"server_id": "p", "server_type": "plex", "library_id": "3", "item_count": 5000,
                 "movie_count": 12, "episode_count": 4321, "show_count": 80, "track_count": 0},
                {"server_id": "p", "server_type": "plex", "library_id": "4", "item_count": 42,
                 "movie_count": 0, "episode_count": 0, "show_count": 0, "track_count": 0}
            ]}),
            &names(),
        );
        let get = |srv: &str, kind: &str| {
            libs.iter()
                .find(|l| l.name == srv && l.kind == kind)
                .map(|l| (l.count, l.children))
        };
        assert_eq!(get("Emby", "track"), Some((Some(177116), None)));
        assert_eq!(get("Plex", "movie"), Some((Some(512), None)));
        assert_eq!(get("Plex", "show"), Some((Some(80), Some(4321))));
        assert_eq!(get("Plex", "other"), Some((Some(42), None)));
        assert_eq!(get("Emby", "movie"), None); // zero counts are left out
    }

    #[test]
    fn today_top_recent() {
        let t = parse_today(
            &json!({"activeStreams": 1, "todayPlays": 7, "todaySessions": 7,
            "watchTimeHours": 1.6, "alertsLast24h": 0, "activeUsersToday": 2}),
        );
        assert_eq!((t.plays, t.active_users, t.active_streams), (7, 2, 1));
        assert!((t.watch_hours - 1.6).abs() < 1e-9);

        let top = parse_top(
            &json!({"data": [{"media_type": "show", "title": "The Cuphead Show!", "year": 2022,
            "plays": 40, "watch_time_ms": 37722415, "unique_users": 1}], "meta": {"nextCursor": null}}),
        );
        assert_eq!(top[0].title, "The Cuphead Show!");
        assert_eq!(
            (top[0].plays, top[0].viewers, top[0].year),
            (40, 1, Some(2022))
        );
        assert!((top[0].watch_hours - 10.478).abs() < 0.01);

        let r = parse_recent(
            &json!({"data": [
                {"server_id": "e", "server_type": "emby", "media_type": "episode", "title": "The High Court",
                 "year": 2026, "added_at": "2026-11-27T16:48:39.000Z", "removed_at": null},
                {"server_id": "p", "media_type": "movie", "title": "Gone", "added_at": "2026-11-26T01:00:00Z",
                 "removed_at": "2026-11-27T00:00:00Z"}
            ]}),
            &names(),
        );
        assert_eq!(r.len(), 1, "removed items are skipped");
        assert_eq!(r[0].added, "2026-11-27 16:48");
        assert_eq!(r[0].server, "Emby");
        assert_eq!(r[0].year, Some(2026));
    }
}
