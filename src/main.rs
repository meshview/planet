// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

use anyhow::Result;
use clap::{Parser, Subcommand};
use meshview_planet::app;
use meshview_planet::config::{self, Config};
use std::path::{Path, PathBuf};
use tracing::info;
use tracing_subscriber::EnvFilter;

/// A world map of the nodes every Meshview instance sees.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// The configuration file. Named here or in MESHVIEW_PLANET_CONFIG it has to
    /// exist; otherwise ./config.toml or /etc/meshview-planet/config.toml is
    /// read, the first that is there.
    #[arg(long, short, env = "MESHVIEW_PLANET_CONFIG", global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the web and collect from the instances (the default).
    Run,
    /// Collect from every instance once, and exit.
    Collect,
    /// Add the instances another site lists, and exit.
    Import {
        /// Where to read the list from, the site's /api/instances.
        source: String,
    },
    /// Create the database or bring its schema up to date, and exit.
    Migrate,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    info!(
        "{} {} — free software under the AGPL, version 3 or later",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );

    let config = match &cli.config {
        Some(path) => Config::load(path, true)?,
        None => {
            let searched = config::SEARCHED.map(Path::new);
            let found = config::first_existing(&searched);
            Config::load(found.unwrap_or(searched[0]), false)?
        }
    };
    match config.source() {
        Some(path) => info!("configuration read from {}", path.display()),
        None => info!("no configuration file; using the defaults and the environment"),
    }
    let from_environment: Vec<&str> = config.environment_variables().collect();
    if !from_environment.is_empty() {
        info!(
            "settings from the environment: {}",
            from_environment.join(", ")
        );
    }

    match cli.command.unwrap_or(Command::Run) {
        Command::Run => app::run(config).await,
        Command::Collect => app::collect(config).await,
        Command::Import { source } => app::import(config, &source).await,
        Command::Migrate => app::migrate(config).await,
    }
}
