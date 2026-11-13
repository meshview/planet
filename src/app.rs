// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Putting the process together.

use crate::collector::{Collector, unix_now};
use crate::config::Config;
use crate::submit::RateLimiter;
use crate::web::{self, AppState};
use crate::{db, import};
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tracing::{error, info, warn};

/// How long requests in flight get to finish once the process is asked to stop.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Serves the web and runs the collector until SIGINT or SIGTERM.
pub async fn run(config: Config) -> Result<()> {
    let (stop, stopping) = watch::channel(false);
    tokio::spawn(wait_for_signal(stop.clone()));
    run_until(config, stopping).await
}

/// Serves the web and runs the collector until `stopping` turns true.
pub async fn run_until(config: Config, stopping: watch::Receiver<bool>) -> Result<()> {
    let pool = db::open(&config.database.path).await?;
    info!("database {}", config.database.path.display());
    let collector = Collector::new(pool.clone(), config.collector.clone())?;

    let addr = config.listen_addr()?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!("listening on http://{addr}");

    let state = AppState {
        pool: pool.clone(),
        collector: collector.clone(),
        limiter: Arc::new(RateLimiter::new(config.submissions.per_ip_per_hour)),
        config: Arc::new(config),
    };
    let mut server_stopping = stopping.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            web::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = server_stopping.wait_for(|stop| *stop).await;
        })
        .await
    });

    collector.run(stopping.clone()).await;

    info!("stopping");
    match tokio::time::timeout(DRAIN_GRACE, server).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => warn!("the web server stopped with an error: {e}"),
        Ok(Err(e)) => warn!("the web server task failed: {e}"),
        Err(_) => warn!("still serving requests after {DRAIN_GRACE:?}; stopping anyway"),
    }
    db::close(pool).await;
    info!("stopped");
    Ok(())
}

/// One collection round, and exit.
pub async fn collect(config: Config) -> Result<()> {
    let pool = db::open(&config.database.path).await?;
    let collector = Collector::new(pool.clone(), config.collector)?;
    let result = collector.round(unix_now()).await;
    db::close(pool).await;
    result.map(|_| ())
}

/// Seeds the list from another site, and exit.
pub async fn import(config: Config, source: &str) -> Result<()> {
    let pool = db::open(&config.database.path).await?;
    let collector = Collector::new(pool.clone(), config.collector.clone())?;
    let result = import::import(
        &pool,
        collector.client(),
        source,
        config.collector.allow_private_addresses,
        unix_now(),
    )
    .await;
    db::close(pool).await;
    let imported = result?;
    info!(
        "{} instances added, {} already there, {} left out",
        imported.added,
        imported.already_there,
        imported.unknown.len()
    );
    Ok(())
}

/// Creates the database or brings its schema up to date, and exit.
pub async fn migrate(config: Config) -> Result<()> {
    let pool = db::open(&config.database.path).await?;
    db::close(pool).await;
    info!("{} is up to date", config.database.path.display());
    Ok(())
}

/// Turns SIGINT or SIGTERM into a stop.
async fn wait_for_signal(stop: watch::Sender<bool>) {
    let (Ok(mut interrupt), Ok(mut terminate)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    ) else {
        error!("could not install the signal handlers");
        return;
    };
    tokio::select! {
        _ = interrupt.recv() => {}
        _ = terminate.recv() => {}
    }
    let _ = stop.send(true);
}
