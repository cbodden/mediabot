//! Seerr (Overseerr / Jellyseerr API v1): search, request, approve/decline, counts.

use anyhow::{Result, bail};
use reqwest::Method;
use serde::Serialize;
use serde_json::{Value, json};

use super::{Service, n, s};

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub media_type: String,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<String>,
    pub status: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestInfo {
    pub id: i64,
    pub status: i64,
    pub media_type: String,
    pub tmdb_id: i64,
    pub title: String,
    pub requested_by: String,
    pub created: String,
}

/// Media availability status (`mediaInfo.status`).
pub fn media_status(code: i64) -> &'static str {
    match code {
        2 => "pending",
        3 => "processing",
        4 => "partially available",
        5 => "available",
        6 => "blocklisted",
        7 => "deleted",
        _ => "unknown",
    }
}

/// Request approval status.
pub fn request_status(code: i64) -> &'static str {
    match code {
        1 => "pending approval",
        2 => "approved",
        3 => "declined",
        4 => "failed",
        5 => "completed",
        _ => "unknown",
    }
}

fn year(date: &str) -> Option<String> {
    (date.len() >= 4).then(|| date[..4].to_string())
}

impl Service {
    pub async fn seerr_counts(&self) -> Result<Vec<(String, i64)>> {
        let v = self.get("/api/v1/request/count", &[]).await?;
        Ok(v.as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, x)| Some((k.clone(), x.as_i64()?)))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn seerr_search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let v = self
            .get(
                "/api/v1/search",
                &[("query", query.into()), ("page", "1".into())],
            )
            .await?;
        let empty = vec![];
        Ok(v["results"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter(|r| matches!(r["mediaType"].as_str(), Some("movie" | "tv")))
            .take(limit)
            .map(|r| {
                let mt = s(r, "mediaType");
                let (title, date) = if mt == "movie" {
                    (s(r, "title"), s(r, "releaseDate"))
                } else {
                    (s(r, "name"), s(r, "firstAirDate"))
                };
                SearchHit {
                    media_type: mt,
                    tmdb_id: n(r, "id").unwrap_or(0.0) as i64,
                    title,
                    year: year(&date),
                    status: r["mediaInfo"]["status"].as_i64(),
                }
            })
            .collect())
    }

    /// Title + year for a TMDB id.
    pub async fn seerr_title(&self, media_type: &str, tmdb_id: i64) -> Result<String> {
        let (path, tkey, dkey) = match media_type {
            "movie" => (format!("/api/v1/movie/{tmdb_id}"), "title", "releaseDate"),
            "tv" => (format!("/api/v1/tv/{tmdb_id}"), "name", "firstAirDate"),
            other => bail!("unknown media type {other}"),
        };
        let v = self.get(&path, &[]).await?;
        Ok(match year(&s(&v, dkey)) {
            Some(y) => format!("{} ({y})", s(&v, tkey)),
            None => s(&v, tkey),
        })
    }

    /// `filter`: all, approved, available, pending, processing, unavailable, failed.
    pub async fn seerr_requests(
        &self,
        filter: &str,
        take: usize,
    ) -> Result<(i64, Vec<RequestInfo>)> {
        let v = self
            .get(
                "/api/v1/request",
                &[
                    ("take", take.to_string()),
                    ("skip", "0".into()),
                    ("filter", filter.into()),
                    ("sort", "added".into()),
                ],
            )
            .await?;
        let total = v["pageInfo"]["results"].as_i64().unwrap_or(0);
        let empty = vec![];
        let mut out = vec![];
        for r in v["results"].as_array().unwrap_or(&empty) {
            let media_type = s(r, "type");
            let tmdb_id = r["media"]["tmdbId"].as_i64().unwrap_or(0);
            let title = self
                .seerr_title(&media_type, tmdb_id)
                .await
                .unwrap_or_else(|_| format!("tmdb:{tmdb_id}"));
            out.push(RequestInfo {
                id: r["id"].as_i64().unwrap_or(0),
                status: r["status"].as_i64().unwrap_or(0),
                media_type,
                tmdb_id,
                title,
                requested_by: first(
                    &r["requestedBy"],
                    &[
                        "displayName",
                        "plexUsername",
                        "jellyfinUsername",
                        "username",
                        "email",
                    ],
                ),
                created: s(r, "createdAt").chars().take(10).collect(),
            });
        }
        Ok((total, out))
    }

    /// Create a request. `seasons` is ignored for movies; empty means all seasons.
    pub async fn seerr_request(
        &self,
        media_type: &str,
        tmdb_id: i64,
        seasons: &[i64],
    ) -> Result<Value> {
        let mut body = json!({ "mediaType": media_type, "mediaId": tmdb_id });
        if media_type == "tv" {
            body["seasons"] = if seasons.is_empty() {
                json!("all")
            } else {
                json!(seasons)
            };
        }
        self.request(Method::POST, "/api/v1/request", &[], Some(body))
            .await
    }

    /// `action` is "approve" or "decline".
    pub async fn seerr_moderate(&self, id: i64, action: &str) -> Result<()> {
        self.request(
            Method::POST,
            &format!("/api/v1/request/{id}/{action}"),
            &[],
            None,
        )
        .await?;
        Ok(())
    }
}

fn first(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| s(v, k))
        .find(|x| !x.is_empty())
        .unwrap_or_else(|| "?".into())
}
