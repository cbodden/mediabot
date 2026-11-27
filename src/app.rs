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
    config::{Config, ServiceKind},
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
