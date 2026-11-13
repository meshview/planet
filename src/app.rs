// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Putting the process together.

use crate::collector::{Collector, unix_now};
use crate::config::Config;
use crate::notify::Notifier;
use crate::term::Style;
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

/// One collection round, and exit, saying what it did.
pub async fn collect(config: Config) -> Result<()> {
    let pool = db::open(&config.database.path).await?;
    let notifier = notifier(&config)?;
    let collector =
        Collector::new(pool.clone(), config.collector.clone())?.with_notifier(notifier.clone());
    let result = collector.round(unix_now()).await;
    notifier.flush().await;
    db::close(pool).await;
    let (report, cleanup) = result?;
    println!(
        "{} instances read: {} answered, {} did not; {} nodes",
        report.succeeded + report.failed,
        report.succeeded,
        report.failed,
        report.nodes
    );
    for url in &cleanup.offline {
        println!("{url} is now offline");
    }
    for url in &cleanup.deleted {
        println!("{url} was deleted, after being offline too long");
    }
    if report.failed > 0 {
        println!("`meshview-planet instance list` tells why those that did not answer failed");
    }
    Ok(())
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
    println!(
        "{} instances added, {} already there, {} left out",
        imported.added,
        imported.already_there,
        imported.unknown.len()
    );
    for url in &imported.unknown {
        println!("left out, with no area known for it: {url}");
    }
    Ok(())
}

/// What the admin asks of the instances.
#[derive(Debug, Clone)]
pub enum InstanceAction {
    List,
    Add {
        url: String,
        country: Option<String>,
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
        InstanceAction::List => print!("{}", instance_table(pool, config).await?),
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
                country.as_deref(),
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

/// Every instance, disabled ones too, grouped by state, for a terminal.
async fn instance_table(pool: &sqlx::SqlitePool, config: &Config) -> Result<String> {
    let counts = db::node_counts(pool).await?;
    let instances = db::instances(pool)
        .await?
        .into_iter()
        .map(|instance| {
            let nodes = counts.get(&instance.id).copied().unwrap_or(0);
            (instance, nodes)
        })
        .collect();
    Ok(admin::instance_list(
        instances,
        unix_now(),
        &config.collector,
        Style::for_stdout(),
    ))
}

/// Creates the database or brings its schema up to date, and exit.
pub async fn migrate(config: Config) -> Result<()> {
    let pool = db::open(&config.database.path).await?;
    db::close(pool).await;
    println!("{} is up to date", config.database.path.display());
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
