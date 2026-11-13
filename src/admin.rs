// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! What the admin does from the command line: list the instances, add one
//! (even one that does not answer yet), and remove, disable or enable one.
//!
//! It works on the database directly, with the server running or not: the web
//! notices by itself, since its answers are kept only while the instance table
//! stays as it was. Nothing here is notified: the admin is the one doing it.
//!
//! A disabled instance stays in the database but is off the site: not shown,
//! not read, not cleaned up, and a submission of it from the web is refused.
//! Removing one deletes it with its nodes and its history, and anyone may add
//! it again.

use crate::areas::{Areas, Country, Language};
use crate::collector::Collector;
use crate::db::{self, Instance};
use crate::meshview;
use crate::net::check_url;
use anyhow::{Result, bail};
use sqlx::SqlitePool;

/// How an added instance went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    /// It answered, with this version and these many nodes.
    Answered { version: String, nodes: usize },
    /// It did not, for this reason, and was added anyway.
    NotAnswering(String),
}

/// Adds an instance, as the web does: a normalised URL, an area from the list,
/// and an answer like a Meshview's. With `force`, one that does not answer is
/// added too, to be read by the rounds; the cleanup treats it as any other,
/// so if it never answers it goes offline and is in time deleted.
pub async fn add(
    pool: &SqlitePool,
    collector: &Collector,
    url: &str,
    country: &str,
    region: Option<&str>,
    force: bool,
    now: i64,
) -> Result<(String, Added)> {
    let url = check_url(url, collector.settings().allow_private_addresses)?;
    let (country, region) = Areas::get()
        .check(country, region)
        .map_err(|e| anyhow::anyhow!("{e} (meshview-planet areas lists them)"))?;
    if let Some(listed) = db::instance_by_url(pool, &url).await? {
        match listed.disabled_at {
            Some(_) => bail!("{url} is there, disabled: enable it instead"),
            None => bail!("{url} is already listed"),
        }
    }
    let outcome = meshview::probe(collector.client(), &url, collector.settings().days_active).await;
    if let Err(error) = &outcome
        && !force
    {
        bail!("{url} {error} (--force adds it anyway)");
    }
    let Some(id) = db::add_instance(pool, &url, &country, region.as_deref(), now).await? else {
        bail!("{url} is already listed");
    };
    let added = match outcome {
        Ok(snapshot) => {
            db::record_success(pool, id, now, &snapshot.version, &snapshot.nodes).await?;
            Added::Answered {
                version: snapshot.version.version,
                nodes: snapshot.nodes.len(),
            }
        }
        Err(error) => {
            let error = error.to_string();
            db::record_failure(pool, id, now, &error).await?;
            Added::NotAnswering(error)
        }
    };
    Ok((url, added))
}

/// The instance at a URL, written as it was added or not.
pub async fn find(pool: &SqlitePool, url: &str) -> Result<Instance> {
    let url = check_url(url, true)?;
    match db::instance_by_url(pool, &url).await? {
        Some(instance) => Ok(instance),
        None => bail!("{url} is not in the database"),
    }
}

/// Deletes an instance with its nodes and its history.
pub async fn remove(pool: &SqlitePool, url: &str) -> Result<String> {
    let instance = find(pool, url).await?;
    db::delete_instance(pool, &instance.url).await?;
    Ok(instance.url)
}

/// Takes an instance off the site, keeping it in the database.
pub async fn disable(pool: &SqlitePool, url: &str, now: i64) -> Result<String> {
    let instance = find(pool, url).await?;
    if instance.disabled_at.is_some() {
        bail!("{} is already disabled", instance.url);
    }
    db::set_disabled(pool, &instance.url, Some(now)).await?;
    Ok(instance.url)
}

/// Puts a disabled instance back on the site, and reads it at once. Answers it
/// as it is after that read.
pub async fn enable(
    pool: &SqlitePool,
    collector: &Collector,
    url: &str,
    now: i64,
) -> Result<Instance> {
    let instance = find(pool, url).await?;
    if instance.disabled_at.is_none() {
        bail!("{} is not disabled", instance.url);
    }
    db::set_disabled(pool, &instance.url, None).await?;
    let instance = find(pool, &instance.url).await?;
    collector.collect_one(instance.clone(), now).await;
    find(pool, &instance.url).await
}

/// What state an instance is in, in a word.
pub fn state(instance: &Instance) -> &'static str {
    if instance.disabled_at.is_some() {
        "disabled"
    } else if instance.offline_since.is_some() {
        "offline"
    } else if instance.last_error.is_some() {
        "failing"
    } else {
        "online"
    }
}

/// The areas to choose from, as text for a terminal. Without a query, every
/// country with its code. With one, a country's regions: the query is its code
/// (`ES`) or part of its English name (`spain`); a name that fits several
/// countries lists them instead. In English, as the rest of the command line.
pub fn areas(query: Option<&str>) -> Result<String> {
    let areas = Areas::get();
    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        return Ok(country_lines(areas.countries().iter()));
    };
    if let Some(country) = areas.country(&query.to_ascii_uppercase()) {
        return Ok(regions(country));
    }
    let wanted = query.to_lowercase();
    let found: Vec<&Country> = areas
        .countries()
        .iter()
        .filter(|country| {
            country
                .name(Language::English)
                .to_lowercase()
                .contains(&wanted)
        })
        .collect();
    match found.as_slice() {
        [] => bail!("no country is called {query:?} (meshview-planet areas lists them)"),
        [country] => Ok(regions(country)),
        several => Ok(country_lines(several.iter().copied())),
    }
}

/// One line per country: its code and its name.
fn country_lines<'a>(countries: impl Iterator<Item = &'a Country>) -> String {
    let mut text = String::new();
    for country in countries {
        text.push_str(&format!(
            "{}  {}\n",
            country.code,
            country.name(Language::English)
        ));
    }
    text
}

/// A country and its regions, one per line, with how to add an instance there.
fn regions(country: &Country) -> String {
    let mut text = country_lines(std::iter::once(country));
    if country.regions.is_empty() {
        text.push_str(&format!(
            "\nNo regions: --country {} alone.\n",
            country.code
        ));
        return text;
    }
    text.push('\n');
    for region in &country.regions {
        text.push_str(&format!("{}  {}\n", region.code, region.name));
    }
    text.push_str(&format!(
        "\n--country {} alone for the whole country, or with --region and one of these.\n",
        country.code
    ));
    text
}
