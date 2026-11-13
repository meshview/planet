// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

use anyhow::Result;
use clap::{ArgAction, Parser, Subcommand};
use meshview_planet::admin;
use meshview_planet::app::{self, InstanceAction};
use meshview_planet::config::{self, Config};
use meshview_planet::term::Style;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use tracing::info;
use tracing_subscriber::EnvFilter;

/// A world map of the nodes every Meshview instance sees.
#[derive(Debug, Parser)]
#[command(version, about, arg_required_else_help = true)]
struct Cli {
    /// The configuration file. Named here or in MESHVIEW_PLANET_CONFIG it has to
    /// exist; otherwise ./config.toml or /etc/meshview-planet/config.toml is
    /// read, the first that is there.
    #[arg(long, short, env = "MESHVIEW_PLANET_CONFIG", global = true)]
    config: Option<PathBuf>,

    /// Show the log: `run` always does; the other commands only with this.
    /// Twice for more detail. RUST_LOG, if set, decides instead.
    #[arg(long, short, action = ArgAction::Count, global = true)]
    verbose: u8,

    /// What to do: there is no default, so that a bare `meshview-planet` only
    /// shows this help rather than start a server.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the web and collect from the instances, until stopped.
    Run,
    /// Collect from every instance once, and exit.
    Collect,
    /// Add the instances another site lists, with their history, and exit.
    Import {
        /// Where to read the list from, the site's /api/instances; the history
        /// is read beside it, from /api/histogram.
        source: String,
    },
    /// Create the database or bring its schema up to date, and exit.
    Migrate,
    /// List the countries to choose from or, given one (by code or name), its
    /// regions, and exit.
    Areas {
        /// A country's code (ES) or part of its English name (spain).
        country: Option<String>,
    },
    /// List, add, remove, disable or enable instances, and exit. It can be
    /// done with the server running: the site follows by itself.
    Instance {
        #[command(subcommand)]
        action: InstanceCommand,
    },
}

#[derive(Debug, Subcommand)]
enum InstanceCommand {
    /// Every instance, disabled ones too, with its state.
    List,
    /// Add an instance, which has to answer as a Meshview unless forced.
    Add {
        /// Its address, as https://meshview.example.org.
        url: String,
        /// The country it is in, as an ISO 3166-1 code (ES): `areas` lists
        /// them. Not needed with --region, which names it already.
        #[arg(long, required_unless_present = "region")]
        country: Option<String>,
        /// The region, as an ISO 3166-2 code (ES-GA): `areas ES` lists them.
        #[arg(long)]
        region: Option<String>,
        /// Add it even if it does not answer now. If it never does, the
        /// cleanup takes it offline and in time deletes it, as any other.
        #[arg(long)]
        force: bool,
    },
    /// Delete an instance, with its nodes and its history. Anyone may add it
    /// again; to keep it off, disable it.
    Remove { url: String },
    /// Take an instance off the site, keeping it in the database: not shown,
    /// not read, and refused if submitted again.
    Disable { url: String },
    /// Put a disabled instance back on the site, and read it at once.
    Enable { url: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // The server's log is what it is for; the other commands say what they
    // did on the standard output, and log only when asked.
    let serving = matches!(cli.command, Command::Run);
    let level = match (serving, cli.verbose) {
        (true, 0) => "info",
        (true, _) | (false, 2..) => "debug",
        (false, 1) => "info",
        (false, 0) => "off",
    };
    let colour = std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && std::io::stderr().is_terminal();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()))
        .with_writer(std::io::stderr)
        .with_ansi(colour)
        .init();

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

    match cli.command {
        Command::Run => app::run(config).await,
        Command::Collect => app::collect(config).await,
        Command::Import { source } => app::import(config, &source).await,
        Command::Migrate => app::migrate(config).await,
        Command::Areas { country } => {
            print!("{}", admin::areas(country.as_deref(), Style::for_stdout())?);
            Ok(())
        }
        Command::Instance { action } => {
            let action = match action {
                InstanceCommand::List => InstanceAction::List,
                InstanceCommand::Add {
                    url,
                    country,
                    region,
                    force,
                } => InstanceAction::Add {
                    url,
                    country,
                    region,
                    force,
                },
                InstanceCommand::Remove { url } => InstanceAction::Remove(url),
                InstanceCommand::Disable { url } => InstanceAction::Disable(url),
                InstanceCommand::Enable { url } => InstanceAction::Enable(url),
            };
            app::instance(config, action).await
        }
    }
}
