//! Sonarr (v3 API), Radarr (v3) and Lidarr (v1): queue, calendar, missing, lookup, health.

use anyhow::{Result, bail};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;

use super::{Service, n, s};
use crate::config::ServiceKind;

#[derive(Debug, Clone, Serialize)]
pub struct QueueItem {
    pub title: String,
    pub status: String,
    pub progress: Option<f64>,
    pub timeleft: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthIssue {
    pub level: String,
    pub message: String,
}

fn sxxexx(v: &Value) -> String {
    match (n(v, "seasonNumber"), n(v, "episodeNumber")) {
        (Some(s), Some(e)) => format!("S{:02}E{:02}", s as i64, e as i64),
        _ => String::new(),
    }
}

fn title_year(v: &Value, title_key: &str) -> String {
    match n(v, "year").filter(|y| *y > 0.0) {
        Some(y) => format!("{} ({})", s(v, title_key), y as i64),
        None => s(v, title_key),
    }
}

fn date_part(iso: &str) -> String {
    iso.chars().take(10).collect()
}

impl Service {
    pub(crate) fn arr_api(&self) -> &'static str {
        if self.kind == ServiceKind::Lidarr {
            "/api/v1"
        } else {
            "/api/v3"
        }
    }

    fn ensure_arr(&self) -> Result<()> {
        if !self.kind.is_arr() {
            bail!("{} is not a sonarr/radarr/lidarr service", self.name);
        }
        Ok(())
    }

    pub async fn arr_queue(&self, limit: usize) -> Result<(i64, Vec<QueueItem>)> {
        self.ensure_arr()?;
        let mut q = vec![
            ("page", "1".to_string()),
            ("pageSize", limit.max(1).to_string()),
        ];
        match self.kind {
            ServiceKind::Sonarr => q.extend([
                ("includeSeries", "true".into()),
                ("includeEpisode", "true".into()),
            ]),
            ServiceKind::Radarr => q.push(("includeMovie", "true".into())),
            ServiceKind::Lidarr => q.extend([
                ("includeArtist", "true".into()),
                ("includeAlbum", "true".into()),
            ]),
            _ => {}
        }
        let v = self.get(&format!("{}/queue", self.arr_api()), &q).await?;
        let total = v["totalRecords"].as_i64().unwrap_or(0);
        let empty = vec![];
        let items = v["records"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .take(limit)
            .map(|r| {
                let nice = match self.kind {
                    ServiceKind::Sonarr if r["series"].is_object() => {
                        format!("{} {}", s(&r["series"], "title"), sxxexx(&r["episode"]))
                    }
                    ServiceKind::Radarr if r["movie"].is_object() => {
                        title_year(&r["movie"], "title")
                    }
                    ServiceKind::Lidarr if r["artist"].is_object() => {
                        format!(
                            "{} - {}",
                            s(&r["artist"], "artistName"),
                            s(&r["album"], "title")
                        )
                    }
                    _ => s(r, "title"),
                };
                let size = n(r, "size").unwrap_or(0.0);
                let left = n(r, "sizeleft").unwrap_or(0.0);
                let state = s(r, "trackedDownloadState");
                QueueItem {
                    title: nice.trim().to_string(),
                    status: if state.is_empty() || state == "downloading" {
                        s(r, "status")
                    } else {
                        state
                    },
                    progress: (size > 0.0)
                        .then(|| ((size - left) / size * 100.0).clamp(0.0, 100.0)),
                    timeleft: Some(s(r, "timeleft")).filter(|t| !t.is_empty()),
                }
            })
            .collect();
        Ok((total, items))
    }

    /// Releases in the next `days` days, formatted one per line.
    pub async fn arr_upcoming(&self, days: i64) -> Result<Vec<String>> {
        self.ensure_arr()?;
        let today = Utc::now().date_naive();
        let end = today + Duration::days(days);
        let mut q = vec![("start", today.to_string()), ("end", end.to_string())];
        match self.kind {
            ServiceKind::Sonarr => q.push(("includeSeries", "true".into())),
            ServiceKind::Lidarr => q.push(("includeArtist", "true".into())),
            _ => {}
        }
        let v = self
            .get(&format!("{}/calendar", self.arr_api()), &q)
            .await?;
        let empty = vec![];
        let mut rows: Vec<(String, String)> = vec![];
        for e in v.as_array().unwrap_or(&empty) {
            match self.kind {
                ServiceKind::Sonarr => {
                    let when = DateTime::parse_from_rfc3339(&s(e, "airDateUtc"))
                        .map(|d| d.with_timezone(&Utc).format("%Y-%m-%d %H:%MZ").to_string())
                        .unwrap_or_else(|_| s(e, "airDate"));
                    let have = if e["hasFile"].as_bool() == Some(true) {
                        " ✓"
                    } else {
                        ""
                    };
                    rows.push((
                        when.clone(),
                        format!(
                            "{when}  {} {} - {}{have}",
                            s(&e["series"], "title"),
                            sxxexx(e),
                            s(e, "title")
                        ),
                    ));
                }
                ServiceKind::Radarr => {
                    // Pick whichever release date falls inside the window.
                    for (key, label) in [
                        ("digitalRelease", "digital"),
                        ("physicalRelease", "physical"),
                        ("inCinemas", "cinemas"),
                    ] {
                        let d = date_part(&s(e, key));
                        if let Ok(nd) = NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                            && nd >= today
                            && nd <= end
                        {
                            rows.push((
                                d.clone(),
                                format!("{d}  {} [{label}]", title_year(e, "title")),
                            ));
                        }
                    }
                }
                ServiceKind::Lidarr => {
                    let d = date_part(&s(e, "releaseDate"));
                    rows.push((
                        d.clone(),
                        format!("{d}  {} - {}", s(&e["artist"], "artistName"), s(e, "title")),
                    ));
                }
                _ => {}
            }
        }
        rows.sort();
        Ok(rows.into_iter().map(|(_, l)| l).collect())
    }

    /// (items managed, monitored-but-missing count).
    pub async fn arr_totals(&self) -> Result<(i64, i64)> {
        self.ensure_arr()?;
        let api = self.arr_api();
        match self.kind {
            ServiceKind::Radarr => {
                let v = self.get(&format!("{api}/movie"), &[]).await?;
                let movies = v.as_array().cloned().unwrap_or_default();
                let missing = movies
                    .iter()
                    .filter(|m| {
                        m["monitored"].as_bool() == Some(true)
                            && m["hasFile"].as_bool() == Some(false)
                            && m["isAvailable"].as_bool().unwrap_or(true)
                    })
                    .count();
                Ok((movies.len() as i64, missing as i64))
            }
            _ => {
                let what = if self.kind == ServiceKind::Sonarr {
                    "series"
                } else {
                    "artist"
                };
                let items = self
                    .get(&format!("{api}/{what}"), &[])
                    .await?
                    .as_array()
                    .map_or(0, |a| a.len());
                let missing = self
                    .get(
                        &format!("{api}/wanted/missing"),
                        &[("pageSize", "1".into()), ("monitored", "true".into())],
                    )
                    .await?["totalRecords"]
                    .as_i64()
                    .unwrap_or(0);
                Ok((items as i64, missing))
            }
        }
    }

    pub async fn arr_missing(&self, limit: usize) -> Result<(i64, Vec<String>)> {
        self.ensure_arr()?;
        let api = self.arr_api();
        match self.kind {
            ServiceKind::Radarr => {
                let v = self.get(&format!("{api}/movie"), &[]).await?;
                let mut missing: Vec<&Value> = v
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|m| {
                                m["monitored"].as_bool() == Some(true)
                                    && m["hasFile"].as_bool() == Some(false)
                                    && m["isAvailable"].as_bool().unwrap_or(true)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                missing.sort_by_key(|m| std::cmp::Reverse(s(m, "added")));
                Ok((
                    missing.len() as i64,
                    missing
                        .iter()
                        .take(limit)
                        .map(|m| title_year(m, "title"))
                        .collect(),
                ))
            }
            _ => {
                let mut q = vec![
                    ("pageSize", limit.to_string()),
                    ("monitored", "true".into()),
                ];
                q.push(if self.kind == ServiceKind::Sonarr {
                    ("includeSeries", "true".into())
                } else {
                    ("includeArtist", "true".into())
                });
                let v = self.get(&format!("{api}/wanted/missing"), &q).await?;
                let total = v["totalRecords"].as_i64().unwrap_or(0);
                let empty = vec![];
                let rows = v["records"]
                    .as_array()
                    .unwrap_or(&empty)
                    .iter()
                    .map(|r| {
                        if self.kind == ServiceKind::Sonarr {
                            format!(
                                "{} {} - {} (aired {})",
                                s(&r["series"], "title"),
                                sxxexx(r),
                                s(r, "title"),
                                date_part(&s(r, "airDateUtc"))
                            )
                        } else {
                            format!(
                                "{} - {} ({})",
                                s(&r["artist"], "artistName"),
                                s(r, "title"),
                                date_part(&s(r, "releaseDate"))
                            )
                        }
                    })
                    .collect();
                Ok((total, rows))
            }
        }
    }

    /// Search the indexer metadata (TVDB/TMDB/MusicBrainz) and flag what's already in the library.
    pub async fn arr_lookup(&self, term: &str, limit: usize) -> Result<Vec<String>> {
        self.ensure_arr()?;
        let what = match self.kind {
            ServiceKind::Sonarr => "series",
            ServiceKind::Radarr => "movie",
            _ => "artist",
        };
        let v = self
            .get(
                &format!("{}/{what}/lookup", self.arr_api()),
                &[("term", term.into())],
            )
            .await?;
        let empty = vec![];
        Ok(v.as_array()
            .unwrap_or(&empty)
            .iter()
            .take(limit)
            .map(|r| {
                let in_lib = n(r, "id").is_some_and(|id| id > 0.0);
                let name = if self.kind == ServiceKind::Lidarr {
                    s(r, "artistName")
                } else {
                    title_year(r, "title")
                };
                let detail = if in_lib {
                    let st = &r["statistics"];
                    match self.kind {
                        ServiceKind::Radarr => {
                            if r["hasFile"].as_bool() == Some(true) {
                                "in library, downloaded".into()
                            } else {
                                "in library, missing".to_string()
                            }
                        }
                        ServiceKind::Sonarr => format!(
                            "in library, {}/{} episodes",
                            n(st, "episodeFileCount").unwrap_or(0.0) as i64,
                            n(st, "totalEpisodeCount")
                                .or(n(st, "episodeCount"))
                                .unwrap_or(0.0) as i64
                        ),
                        _ => format!(
                            "in library, {} tracks",
                            n(st, "trackFileCount").unwrap_or(0.0) as i64
                        ),
                    }
                } else {
                    "not in library".to_string()
                };
                format!("{name} — {detail}")
            })
            .collect())
    }

    pub async fn arr_health(&self) -> Result<Vec<HealthIssue>> {
        self.ensure_arr()?;
        let v = self.get(&format!("{}/health", self.arr_api()), &[]).await?;
        let empty = vec![];
        Ok(v.as_array()
            .unwrap_or(&empty)
            .iter()
            .map(|h| HealthIssue {
                level: s(h, "type"),
                message: s(h, "message"),
            })
            .collect())
    }
}
