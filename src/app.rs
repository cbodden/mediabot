//! Shared application state.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Instant,
};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, RwLock};

use crate::{
    acl::Store,
    config::{Config, MediaSource, ServiceKind},
    irc::{IrcHandle, IrcStatus},
    logging::LogBuffer,
    metrics::Metrics,
    poller::ServiceStatus,
    services::Service,
};

pub struct App {
    pub config: Config,
    pub store: Store,
    pub metrics: Arc<Metrics>,
    pub services: Vec<Arc<Service>>,
    pub irc: IrcHandle,
    pub irc_status: RwLock<IrcStatus>,
    pub service_status: RwLock<BTreeMap<String, ServiceStatus>>,
    pub logs: Arc<LogBuffer>,
    pub cooldowns: Mutex<HashMap<String, Instant>>,
    pub started: DateTime<Utc>,
}

impl App {
    /// Services matching `pred`, optionally narrowed by `arg` (a service name or kind).
    pub fn select(
        &self,
        pred: impl Fn(ServiceKind) -> bool,
        arg: Option<&str>,
        what: &str,
    ) -> Result<Vec<Arc<Service>>> {
        let all: Vec<_> = self
            .services
            .iter()
            .filter(|s| pred(s.kind))
            .cloned()
            .collect();
        if all.is_empty() {
            bail!("no {what} services are configured");
        }
        let Some(arg) = arg else { return Ok(all) };
        let arg = arg.to_ascii_lowercase();
        let picked: Vec<_> = all
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(&arg) || s.kind.as_str() == arg)
            .cloned()
            .collect();
        if picked.is_empty() {
            let names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
            bail!(
                "unknown {what} service {arg:?} (have: {})",
                names.join(", ")
            );
        }
        Ok(picked)
    }

    /// Media services for `np` / `libs`: (ask these, then these if every one of the first fails).
    /// A named service always wins; otherwise [`pick_media`] decides from `[bot] media_source`.
    pub async fn media_targets(
        &self,
        arg: Option<&str>,
    ) -> Result<(Vec<Arc<Service>>, Vec<Arc<Service>>)> {
        if arg.is_some() {
            return Ok((
                self.select(ServiceKind::is_media_server, arg, "media server")?,
                vec![],
            ));
        }
        let st = self.service_status.read().await;
        // a Tracearr that hasn't been polled yet counts as available - the fallback covers it
        let items: Vec<(ServiceKind, bool)> = self
            .services
            .iter()
            .map(|s| {
                let avail = s.kind == ServiceKind::Tracearr
                    && st
                        .get(&s.name)
                        .is_none_or(|x| x.up || x.last_poll.is_none());
                (s.kind, avail)
            })
            .collect();
        drop(st);
        let (p, f) = pick_media(self.config.bot.media_source, &items);
        if p.is_empty() {
            bail!(
                "no media servers for media_source = {:?} (have: {})",
                self.config.bot.media_source,
                self.services
                    .iter()
                    .filter(|s| s.kind.is_media_server())
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let get = |ix: Vec<usize>| ix.into_iter().map(|i| self.services[i].clone()).collect();
        Ok((get(p), get(f)))
    }

    /// True when the direct media servers should sit on standby (not polled, not asked):
    /// `tracearr` mode, or `auto` while a Tracearr service is up.
    pub fn direct_on_standby(&self, tracearr_up: bool) -> bool {
        match self.config.bot.media_source {
            MediaSource::Tracearr => true,
            MediaSource::Auto => tracearr_up,
            MediaSource::Direct | MediaSource::All => false,
        }
    }

    pub fn is_service_word(&self, w: &str) -> bool {
        self.services
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(w) || s.kind.as_str().eq_ignore_ascii_case(w))
    }

    pub async fn refresh_user_metrics(&self) {
        for (role, n) in self.store.user_counts().await {
            self.metrics
                .users
                .with_label_values(&[role.as_str()])
                .set(n as i64);
        }
    }
}

/// Decide which media services answer `np` / `libs` when none is named. `items` is every service
/// as (kind, "is a Tracearr that's available"). Returns indexes: (primary, fallback).
pub fn pick_media(mode: MediaSource, items: &[(ServiceKind, bool)]) -> (Vec<usize>, Vec<usize>) {
    let idx = |f: &dyn Fn(ServiceKind, bool) -> bool| -> Vec<usize> {
        items
            .iter()
            .enumerate()
            .filter(|(_, (k, a))| f(*k, *a))
            .map(|(i, _)| i)
            .collect()
    };
    let tracearr = idx(&|k, _| k == ServiceKind::Tracearr);
    let direct = idx(&|k, _| k.is_direct_media());
    match mode {
        MediaSource::Direct => (direct, vec![]),
        MediaSource::Tracearr => (tracearr, vec![]),
        MediaSource::All => (idx(&|k, _| k.is_media_server()), vec![]),
        MediaSource::Auto => {
            let up = idx(&|k, a| k == ServiceKind::Tracearr && a);
            if !up.is_empty() {
                (up, direct)
            } else if direct.is_empty() {
                (tracearr, vec![]) // only Tracearr configured: still try it
            } else {
                (direct, vec![])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ServiceKind::*;

    #[test]
    fn media_source_choice() {
        // plex, jellyfin, tracearr (available), sonarr
        let up = [
            (Plex, false),
            (Jellyfin, false),
            (Tracearr, true),
            (Sonarr, false),
        ];
        assert_eq!(pick_media(MediaSource::Auto, &up), (vec![2], vec![0, 1]));
        assert_eq!(pick_media(MediaSource::Direct, &up), (vec![0, 1], vec![]));
        assert_eq!(pick_media(MediaSource::Tracearr, &up), (vec![2], vec![]));
        assert_eq!(pick_media(MediaSource::All, &up), (vec![0, 1, 2], vec![]));

        // tracearr down: auto uses the direct servers
        let down = [(Plex, false), (Tracearr, false)];
        assert_eq!(pick_media(MediaSource::Auto, &down), (vec![0], vec![]));
        // only tracearr, and it's down: still try it
        assert_eq!(
            pick_media(MediaSource::Auto, &[(Tracearr, false)]),
            (vec![0], vec![])
        );
        // no tracearr at all: unchanged behaviour
        assert_eq!(
            pick_media(MediaSource::Auto, &[(Plex, false), (Emby, false)]),
            (vec![0, 1], vec![])
        );
    }
}
