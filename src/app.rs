// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Putting the process together.

use crate::areas::{Areas, Language};
use crate::collector::{Collector, unix_now};
use crate::config::Config;
use crate::notify::Notifier;
use crate::web::{self, AppState};
use crate::{admin, db, import};
use anyhow::{Context, Result};
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
    let notifier = notifier(&config)?;
    let collector =
        Collector::new(pool.clone(), config.collector.clone())?.with_notifier(notifier.clone());

    let addr = config.listen_addr()?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!("listening on http://{addr}");

    let state = AppState::new(pool.clone(), collector.clone(), config);
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
    notifier.flush().await;
    db::close(pool).await;
    info!("stopped");
    Ok(())
}

/// The notifier the configuration asks for, said in the log.
fn notifier(config: &Config) -> Result<Notifier> {
    let notifier = Notifier::new(&config.notifications)?;
    let channels: Vec<&str> = notifier.channels().iter().map(|c| c.name()).collect();
    if !channels.is_empty() {
        let events: Vec<&str> = config
            .notifications
            .events
            .iter()
            .map(|event| event.as_str())
            .collect();
        info!("notifying {} of {}", channels.join(", "), events.join(", "));
    }
    Ok(notifier)
}

/// One collection round, and exit.
pub async fn collect(config: Config) -> Result<()> {
    let pool = db::open(&config.database.path).await?;
    let notifier = notifier(&config)?;
    let collector =
        Collector::new(pool.clone(), config.collector.clone())?.with_notifier(notifier.clone());
    let result = collector.round(unix_now()).await;
    notifier.flush().await;
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

/// What the admin asks of the instances.
#[derive(Debug, Clone)]
pub enum InstanceAction {
    List,
    Add {
        url: String,
        country: String,
        region: Option<String>,
        force: bool,
    },
    Remove(String),
    Disable(String),
    Enable(String),
}

/// Lists, adds, removes, disables or enables instances, and exit. What it did
/// goes to the standard output; the log, to the standard error.
pub async fn instance(config: Config, action: InstanceAction) -> Result<()> {
    let pool = db::open(&config.database.path).await?;
    let result = instance_action(&pool, &config, action).await;
    db::close(pool).await;
    result
}

async fn instance_action(
    pool: &sqlx::SqlitePool,
    config: &Config,
    action: InstanceAction,
) -> Result<()> {
    let collector = || Collector::new(pool.clone(), config.collector.clone());
    let now = unix_now();
    match action {
        InstanceAction::List => print!("{}", instance_table(pool).await?),
        InstanceAction::Add {
            url,
            country,
            region,
            force,
        } => {
            let (url, added) = admin::add(
                pool,
                &collector()?,
                &url,
                &country,
                region.as_deref(),
                force,
                now,
            )
            .await?;
            match added {
                admin::Added::Answered { version, nodes } => {
                    println!("{url} added: Meshview {version}, {nodes} nodes");
                }
                admin::Added::NotAnswering(error) => {
                    println!("{url} added, not answering yet: {error}");
                }
            }
        }
        InstanceAction::Remove(url) => {
            let url = admin::remove(pool, &url).await?;
            println!("{url} removed, with its nodes and its history");
        }
        InstanceAction::Disable(url) => {
            let url = admin::disable(pool, &url, now).await?;
            println!("{url} disabled: off the site, and refused if submitted again");
        }
        InstanceAction::Enable(url) => {
            let instance = admin::enable(pool, &collector()?, &url, now).await?;
            match &instance.last_error {
                None => println!("{} enabled, and it answers", instance.url),
                Some(error) => println!("{} enabled, not answering yet: {error}", instance.url),
            }
        }
    }
    Ok(())
}

/// Every instance, disabled ones too, as a table for a terminal.
async fn instance_table(pool: &sqlx::SqlitePool) -> Result<String> {
    let mut rows = vec![[
        "STATE".to_string(),
        "NODES".to_string(),
        "VERSION".to_string(),
        "AREA".to_string(),
        "URL".to_string(),
        "LAST ERROR".to_string(),
    ]];
    for instance in db::instances(pool).await? {
        rows.push([
            admin::state(&instance).to_string(),
            db::count_nodes(pool, instance.id).await?.to_string(),
            instance.version.clone().unwrap_or_else(|| "-".into()),
            Areas::get().label(
                &instance.country,
                instance.region.as_deref(),
                Language::English,
            ),
            instance.url.clone(),
            instance.last_error.clone().unwrap_or_default(),
        ]);
    }
    let mut widths = [0; 6];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut table = String::new();
    for row in &rows {
        let mut line = String::new();
        for (i, (cell, width)) in row.iter().zip(widths).enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            if i == 1 {
                line.push_str(&format!("{cell:>width$}"));
            } else {
                line.push_str(&format!("{cell:<width$}"));
            }
        }
        table.push_str(line.trim_end());
        table.push('\n');
    }
    Ok(table)
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
