//! mediabot — an IRC bot for Libera.Chat that reports on Plex/Emby/Jellyfin/Tautulli,
//! Seerr and Sonarr/Radarr/Lidarr, with a local web panel and Prometheus/Loki telemetry.

mod acl;
mod app;
mod commands;
mod config;
mod irc;
mod logging;
mod metrics;
mod poller;
mod services;
mod web;

use std::{collections::BTreeMap, collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to the TOML config file.
    #[arg(short, long, env = "MEDIABOT_CONFIG", default_value = "config.toml")]
    config: PathBuf,
    /// Validate the config and exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = config::Config::load(&cli.config)?;
    if cli.check {
        println!(
            "config OK: {} service(s), {} channel(s), web panel {}",
            config.services.len(),
            config.irc.channels.len(),
            if config.web.enabled {
                "enabled"
            } else {
                "disabled"
            }
        );
        return Ok(());
    }

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("installing rustls crypto provider"))?;

    let logs = logging::LogBuffer::new(1000);
    let loki_task = logging::init(&config.telemetry, logs.clone())?;
    if let Some(t) = loki_task {
        tokio::spawn(t);
        info!("shipping logs to Loki");
    }

    let metrics = Arc::new(metrics::Metrics::new());
    let services = config
        .services
        .iter()
        .map(|s| services::Service::new(s, metrics.clone()).map(Arc::new))
        .collect::<Result<Vec<_>>>()
        .context("building service clients")?;
    let store = acl::Store::open(&config.bot.data_dir, &config.bot.owners)?;
    let (irc, irc_rx) = irc::IrcHandle::new();
    let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();

    let app = Arc::new(app::App {
        config,
        store,
        metrics,
        services,
        irc,
        irc_status: RwLock::new(Default::default()),
        service_status: RwLock::new(BTreeMap::new()),
        logs,
        cooldowns: Mutex::new(HashMap::new()),
        started: chrono::Utc::now(),
    });
    app.refresh_user_metrics().await;
    info!(
        "mediabot {} starting: {} service(s), owners: {}",
        env!("CARGO_PKG_VERSION"),
        app.services.len(),
        app.store.owners().join(", ")
    );

    tokio::spawn(poller::run(app.clone()));
    tokio::spawn(commands::run(app.clone(), chat_rx));
    tokio::spawn(irc::run(app.clone(), irc_rx, chat_tx));

    let web = async {
        if app.config.web.enabled
            && let Err(e) = web::serve(app.clone()).await
        {
            error!("web panel failed: {e:#}");
            return Err(e);
        }
        std::future::pending::<Result<()>>().await
    };

    tokio::select! {
        r = web => r?,
        _ = shutdown_signal() => {
            info!("shutting down");
            app.irc.send(irc::IrcCmd::Raw("QUIT :shutting down".into()));
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await.ok();
}
