//! Plex, Emby, Jellyfin and Tautulli: now-playing sessions and library counts.

use anyhow::{Result, bail};
use serde::Serialize;

use super::{Service, n, s, tautulli_data};
use crate::config::ServiceKind;

#[derive(Debug, Clone, Serialize)]
pub struct Stream {
    pub user: String,
    pub title: String,
    pub paused: bool,
    pub progress: Option<f64>,
    pub transcode: bool,
    pub player: String,
    pub quality: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Activity {
    pub streams: Vec<Stream>,
    pub bandwidth_kbps: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Library {
    pub name: String,
    /// movie / show / artist / photo / ... (normalized lowercase)
    pub kind: String,
    pub count: u64,
    /// Episodes for shows, tracks for music, when known.
    pub children: Option<u64>,
}

fn pct(pos: Option<f64>, dur: Option<f64>) -> Option<f64> {
    match (pos, dur) {
        (Some(p), Some(d)) if d > 0.0 => Some((p / d * 100.0).clamp(0.0, 100.0)),
        _ => None,
    }
}

fn episode_title(series: &str, season: Option<f64>, ep: Option<f64>, name: &str) -> String {
    match (season, ep) {
        (Some(s), Some(e)) => format!("{series} S{:02}E{:02} - {name}", s as i64, e as i64),
        _ => format!("{series} - {name}"),
    }
}

impl Service {
    pub async fn activity(&self) -> Result<Activity> {
        match self.kind {
            ServiceKind::Plex => self.plex_activity().await,
            ServiceKind::Emby | ServiceKind::Jellyfin => self.emby_activity().await,
            ServiceKind::Tautulli => self.tautulli_activity().await,
            k => bail!("{} is not a media server", k.as_str()),
        }
    }

    pub async fn libraries(&self) -> Result<Vec<Library>> {
        match self.kind {
            ServiceKind::Plex => self.plex_libraries().await,
            ServiceKind::Emby | ServiceKind::Jellyfin => self.emby_libraries().await,
            ServiceKind::Tautulli => self.tautulli_libraries().await,
            k => bail!("{} is not a media server", k.as_str()),
        }
    }

    // ---- Plex -------------------------------------------------------------------------

    async fn plex_activity(&self) -> Result<Activity> {
        let v = self.get("/status/sessions", &[]).await?;
        let empty = vec![];
        let items = v["MediaContainer"]["Metadata"].as_array().unwrap_or(&empty);
        let mut act = Activity::default();
        let mut bw = 0.0;
        for m in items {
            let title = match s(m, "type").as_str() {
                "episode" => episode_title(
                    &s(m, "grandparentTitle"),
                    n(m, "parentIndex"),
                    n(m, "index"),
                    &s(m, "title"),
                ),
                "track" => format!("{} - {}", s(m, "grandparentTitle"), s(m, "title")),
                "movie" => match n(m, "year") {
                    Some(y) => format!("{} ({})", s(m, "title"), y as i64),
                    None => s(m, "title"),
                },
                _ => s(m, "title"),
            };
            let transcode = m["TranscodeSession"].is_object()
                && (s(&m["TranscodeSession"], "videoDecision") == "transcode"
                    || s(&m["TranscodeSession"], "audioDecision") == "transcode");
            bw += n(&m["Session"], "bandwidth").unwrap_or(0.0);
            act.streams.push(Stream {
                user: s(&m["User"], "title"),
                title,
                paused: s(&m["Player"], "state") == "paused",
                progress: pct(n(m, "viewOffset"), n(m, "duration")),
                transcode,
                player: first_nonempty(&[s(&m["Player"], "title"), s(&m["Player"], "product")]),
                quality: m["Media"][0]["videoResolution"].as_str().map(|r| {
                    if r.chars().all(|c| c.is_ascii_digit()) {
                        format!("{r}p")
                    } else {
                        r.to_string()
                    }
                }),
            });
        }
        act.bandwidth_kbps = Some(bw);
        Ok(act)
    }

    async fn plex_libraries(&self) -> Result<Vec<Library>> {
        let v = self.get("/library/sections", &[]).await?;
        let empty = vec![];
        let mut out = vec![];
        for d in v["MediaContainer"]["Directory"]
            .as_array()
            .unwrap_or(&empty)
        {
            let key = s(d, "key");
            let kind = s(d, "type");
            let path = format!("/library/sections/{key}/all");
            let size = [
                ("X-Plex-Container-Start", "0".to_string()),
                ("X-Plex-Container-Size", "0".to_string()),
            ];
            let count = n(
                &self.get(&path, &size).await?["MediaContainer"],
                "totalSize",
            )
            .unwrap_or(0.0) as u64;
            // Plex metadata types: 4 = episode, 10 = track.
            let child_type = match kind.as_str() {
                "show" => Some("4"),
                "artist" => Some("10"),
                _ => None,
            };
            let children = match child_type {
                Some(t) => {
                    let mut q = size.to_vec();
                    q.push(("type", t.to_string()));
                    n(&self.get(&path, &q).await?["MediaContainer"], "totalSize").map(|x| x as u64)
                }
                None => None,
            };
            out.push(Library {
                name: s(d, "title"),
                kind,
                count,
                children,
            });
        }
        Ok(out)
    }

    // ---- Emby / Jellyfin --------------------------------------------------------------

    async fn emby_activity(&self) -> Result<Activity> {
        let v = self
            .get("/Sessions", &[("ActiveWithinSeconds", "960".into())])
            .await?;
        let empty = vec![];
        let mut act = Activity::default();
        let mut bw = 0.0;
        let mut have_bw = false;
        for sess in v.as_array().unwrap_or(&empty) {
            let item = &sess["NowPlayingItem"];
            if !item.is_object() {
                continue;
            }
            let title = match s(item, "Type").as_str() {
                "Episode" => episode_title(
                    &s(item, "SeriesName"),
                    n(item, "ParentIndexNumber"),
                    n(item, "IndexNumber"),
                    &s(item, "Name"),
                ),
                "Audio" => format!(
                    "{} - {}",
                    first_nonempty(&[s(item, "AlbumArtist"), s(item, "Album")]),
                    s(item, "Name")
                ),
                "Movie" => match n(item, "ProductionYear") {
                    Some(y) => format!("{} ({})", s(item, "Name"), y as i64),
                    None => s(item, "Name"),
                },
                _ => s(item, "Name"),
            };
            let ps = &sess["PlayState"];
            if let Some(b) = n(&sess["TranscodingInfo"], "Bitrate") {
                bw += b / 1000.0;
                have_bw = true;
            }
            let height = item["MediaStreams"]
                .as_array()
                .and_then(|a| a.iter().find(|st| s(st, "Type") == "Video"))
                .and_then(|st| n(st, "Height"));
            act.streams.push(Stream {
                user: s(sess, "UserName"),
                title,
                paused: ps["IsPaused"].as_bool().unwrap_or(false),
                progress: pct(n(ps, "PositionTicks"), n(item, "RunTimeTicks")),
                transcode: s(ps, "PlayMethod") == "Transcode",
                player: first_nonempty(&[s(sess, "DeviceName"), s(sess, "Client")]),
                quality: height.map(|h| format!("{}p", h as i64)),
            });
        }
        act.bandwidth_kbps = have_bw.then_some(bw);
        Ok(act)
    }

    async fn emby_libraries(&self) -> Result<Vec<Library>> {
        let v = self.get("/Items/Counts", &[]).await?;
        let pairs = [
            ("Movies", "movie", "MovieCount", None),
            ("Shows", "show", "SeriesCount", Some("EpisodeCount")),
            ("Music", "artist", "ArtistCount", Some("SongCount")),
            ("Albums", "album", "AlbumCount", None),
            ("Music Videos", "musicvideo", "MusicVideoCount", None),
            ("Books", "book", "BookCount", None),
            ("Box Sets", "boxset", "BoxSetCount", None),
        ];
        Ok(pairs
            .iter()
            .filter_map(|(name, kind, key, child)| {
                let count = n(&v, key)? as u64;
                (count > 0).then(|| Library {
                    name: name.to_string(),
                    kind: kind.to_string(),
                    count,
                    children: child.and_then(|c| n(&v, c)).map(|x| x as u64),
                })
            })
            .collect())
    }

    // ---- Tautulli ---------------------------------------------------------------------

    async fn tautulli_activity(&self) -> Result<Activity> {
        let v = self
            .get("/api/v2", &[("cmd", "get_activity".into())])
            .await?;
        let d = tautulli_data(&v)?;
        let empty = vec![];
        let streams = d["sessions"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(|x| Stream {
                user: first_nonempty(&[s(x, "friendly_name"), s(x, "user")]),
                title: first_nonempty(&[s(x, "full_title"), s(x, "title")]),
                paused: s(x, "state") == "paused",
                progress: n(x, "progress_percent"),
                transcode: s(x, "transcode_decision") == "transcode",
                player: first_nonempty(&[s(x, "player"), s(x, "product")]),
                quality: Some(first_nonempty(&[
                    s(x, "stream_video_full_resolution"),
                    s(x, "quality_profile"),
                ]))
                .filter(|q| !q.is_empty()),
            })
            .collect();
        Ok(Activity {
            streams,
            bandwidth_kbps: n(d, "total_bandwidth"),
        })
    }

    async fn tautulli_libraries(&self) -> Result<Vec<Library>> {
        let v = self
            .get("/api/v2", &[("cmd", "get_libraries".into())])
            .await?;
        let empty = vec![];
        Ok(tautulli_data(&v)?
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(|l| {
                let kind = s(l, "section_type");
                Library {
                    name: s(l, "section_name"),
                    children: match kind.as_str() {
                        "show" | "artist" => n(l, "child_count").map(|x| x as u64),
                        _ => None,
                    },
                    count: n(l, "count").unwrap_or(0.0) as u64,
                    kind,
                }
            })
            .collect())
    }
}

fn first_nonempty(opts: &[String]) -> String {
    opts.iter()
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_default()
}
