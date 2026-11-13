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
use crate::config::Collector as Settings;
use crate::db::{self, Instance};
use crate::meshview;
use crate::net::check_url;
use crate::term::{Color, Style, ago};
use anyhow::{Result, bail};
use sqlx::SqlitePool;
use std::collections::BTreeMap;

const DAY: i64 = 86_400;

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

/// What state an instance is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// It answered the latest round.
    Online,
    /// It did not, but has not been silent long enough to go offline.
    Failing,
    /// Silent long enough: off the site, and deleted in time.
    Offline,
    /// Kept off the site by the admin.
    Disabled,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Failing => "failing",
            Self::Offline => "offline",
            Self::Disabled => "disabled",
        }
    }
}

/// What state an instance is in.
pub fn state(instance: &Instance) -> State {
    if instance.disabled_at.is_some() {
        State::Disabled
    } else if instance.offline_since.is_some() {
        State::Offline
    } else if instance.last_error.is_some() {
        State::Failing
    } else {
        State::Online
    }
}

/// How a cell is painted.
#[derive(Debug, Clone, Copy)]
enum Paint {
    Plain,
    Dim,
    Red,
}

/// A cell: its text, how it is painted, and whether it goes to the right.
#[derive(Debug, Clone)]
struct Cell {
    text: String,
    paint: Paint,
    right: bool,
}

impl Cell {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            paint: Paint::Plain,
            right: false,
        }
    }

    fn dim(mut self) -> Self {
        self.paint = Paint::Dim;
        self
    }

    fn red(mut self) -> Self {
        self.paint = Paint::Red;
        self
    }

    fn right(mut self) -> Self {
        self.right = true;
        self
    }
}

/// The columns every state shows, first.
const COMMON: [&str; 4] = ["NODES", "VERSION", "AREA", "URL"];

/// Every instance, grouped by state (online, failing, offline, disabled), each
/// group by nodes, as text for a terminal, with each one's node count. The
/// settings tell when failing ones go offline and offline ones are deleted.
pub fn instance_list(
    instances: Vec<(Instance, i64)>,
    now: i64,
    settings: &Settings,
    style: Style,
) -> String {
    let delete_after_days = settings.delete_after_days;
    if instances.is_empty() {
        return "No instances yet: add one with `meshview-planet instance add`.\n".to_string();
    }
    let total = instances.len();
    let mut groups: BTreeMap<State, Vec<(Instance, i64)>> = BTreeMap::new();
    for (instance, nodes) in instances {
        groups
            .entry(state(&instance))
            .or_default()
            .push((instance, nodes));
    }

    // The common columns are as wide in every group, so they line up.
    let mut rows: Vec<(State, Vec<Vec<Cell>>)> = Vec::new();
    let mut common_widths = COMMON.map(str::len);
    for (state, mut members) in groups {
        members.sort_by(|(a, a_nodes), (b, b_nodes)| b_nodes.cmp(a_nodes).then(a.url.cmp(&b.url)));
        let group: Vec<Vec<Cell>> = members
            .iter()
            .map(|(instance, nodes)| row(state, instance, *nodes, now, settings))
            .collect();
        for cells in &group {
            for (width, cell) in common_widths.iter_mut().zip(cells) {
                *width = (*width).max(cell.text.chars().count());
            }
        }
        rows.push((state, group));
    }

    let mut text = String::new();
    let mut counts = Vec::new();
    for (state, group) in &rows {
        let (color, note) = match state {
            State::Online => (Color::Green, String::new()),
            State::Failing => (
                Color::Yellow,
                "did not answer the latest round; still on the site".to_string(),
            ),
            State::Offline => (
                Color::Red,
                format!("off the site; deleted after {delete_after_days} days offline"),
            ),
            State::Disabled => (
                Color::Grey,
                "off the site and not read; refused if submitted again".to_string(),
            ),
        };
        counts.push(format!("{} {}", group.len(), state.name()));
        if !text.is_empty() {
            text.push('\n');
        }
        let title = format!("● {} ({})", capitalised(state.name()), group.len());
        text.push_str(&style.heading(color, &title));
        if !note.is_empty() {
            text.push_str(&format!("  {}", style.dim(&note)));
        }
        text.push('\n');
        let headers: Vec<Cell> = COMMON
            .iter()
            .chain(extra_columns(*state))
            .enumerate()
            .map(|(i, title)| {
                let cell = Cell::new(*title).dim();
                if i == 0 { cell.right() } else { cell }
            })
            .collect();
        let mut widths: Vec<usize> = common_widths.to_vec();
        for (i, title) in extra_columns(*state).iter().enumerate() {
            let longest = group
                .iter()
                .map(|cells| cells[COMMON.len() + i].text.chars().count())
                .max()
                .unwrap_or(0);
            widths.push(longest.max(title.len()));
        }
        for cells in std::iter::once(&headers).chain(group) {
            text.push_str(&line(cells, &widths, style));
        }
    }
    text.push('\n');
    text.push_str(&style.dim(&format!("{total} instances: {}", counts.join(", "))));
    text.push('\n');
    text
}

/// The columns a state shows after the common ones.
fn extra_columns(state: State) -> &'static [&'static str] {
    match state {
        State::Online => &["READ"],
        State::Failing => &["LAST ANSWER", "OFFLINE", "ERROR"],
        State::Offline => &["OFFLINE", "DELETED", "ERROR"],
        State::Disabled => &["DISABLED"],
    }
}

/// An instance's cells, for its state.
fn row(state: State, instance: &Instance, nodes: i64, now: i64, settings: &Settings) -> Vec<Cell> {
    let area = Areas::get().label(
        &instance.country,
        instance.region.as_deref(),
        Language::English,
    );
    let mut cells = vec![
        Cell::new(nodes.to_string()).right(),
        Cell::new(instance.version.clone().unwrap_or_else(|| "-".into())),
        Cell::new(area),
        Cell::new(instance.url.clone()),
    ];
    let error = || Cell::new(instance.last_error.clone().unwrap_or_default()).red();
    let since = |moment: Option<i64>| match moment {
        Some(moment) => Cell::new(ago(now - moment)),
        None => Cell::new("never").dim(),
    };
    match state {
        State::Online => cells.push(since(instance.last_success_at).dim()),
        State::Failing => {
            cells.push(since(instance.last_success_at));
            let silent_since = instance.last_success_at.unwrap_or(instance.added_at);
            let goes_at = silent_since + i64::from(settings.offline_after_days) * DAY;
            cells.push(Cell::new(days_from_now(goes_at - now)));
            cells.push(error());
        }
        State::Offline => {
            let offline_since = instance.offline_since.unwrap_or(now);
            let gone_at = offline_since + i64::from(settings.delete_after_days) * DAY;
            cells.push(Cell::new(ago(now - offline_since)));
            cells.push(Cell::new(days_from_now(gone_at - now)));
            cells.push(error());
        }
        State::Disabled => cells.push(since(instance.disabled_at)),
    }
    cells
}

/// One line of a table, its cells padded to the widths and painted.
fn line(cells: &[Cell], widths: &[usize], style: Style) -> String {
    let last = cells.len() - 1;
    let mut text = String::from("  ");
    for (i, (cell, &width)) in cells.iter().zip(widths).enumerate() {
        let pad = width.saturating_sub(cell.text.chars().count());
        let painted = match cell.paint {
            Paint::Plain => cell.text.clone(),
            Paint::Dim => style.dim(&cell.text),
            Paint::Red => style.red(&cell.text),
        };
        if cell.right {
            text.push_str(&" ".repeat(pad));
            text.push_str(&painted);
        } else {
            text.push_str(&painted);
            if i < last {
                text.push_str(&" ".repeat(pad));
            }
        }
        if i < last {
            text.push_str("  ");
        }
    }
    text.truncate(text.trim_end().len());
    text.push('\n');
    text
}

/// When something due in `seconds` happens, in whole days: "in 3 days", or
/// "next round" once due.
fn days_from_now(seconds: i64) -> String {
    match seconds.max(0).unsigned_abs().div_ceil(DAY.unsigned_abs()) {
        0 => "next round".to_string(),
        1 => "in 1 day".to_string(),
        days => format!("in {days} days"),
    }
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The areas to choose from, as text for a terminal. Without a query, every
/// country with its code. With one, a country's regions: the query is its code
/// (`ES`) or part of its English name (`spain`); a name that fits several
/// countries lists them instead. In English, as the rest of the command line.
pub fn areas(query: Option<&str>, style: Style) -> Result<String> {
    let areas = Areas::get();
    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        return Ok(country_lines(areas.countries().iter(), style));
    };
    if let Some(country) = areas.country(&query.to_ascii_uppercase()) {
        return Ok(regions(country, style));
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
        [country] => Ok(regions(country, style)),
        several => Ok(country_lines(several.iter().copied(), style)),
    }
}

/// One line per country: its code and its name.
fn country_lines<'a>(countries: impl Iterator<Item = &'a Country>, style: Style) -> String {
    let mut text = String::new();
    for country in countries {
        text.push_str(&format!(
            "{}  {}\n",
            style.cyan(&country.code),
            country.name(Language::English)
        ));
    }
    text
}

/// A country and its regions, one per line, with how to add an instance there.
fn regions(country: &Country, style: Style) -> String {
    let mut text = style.bold(&format!(
        "{}  {}",
        country.code,
        country.name(Language::English)
    ));
    text.push('\n');
    if country.regions.is_empty() {
        let hint = format!("No regions: --country {} alone.", country.code);
        text.push_str(&format!("\n{}\n", style.dim(&hint)));
        return text;
    }
    text.push('\n');
    let width = country
        .regions
        .iter()
        .map(|region| region.code.chars().count())
        .max()
        .unwrap_or(0);
    for region in &country.regions {
        let pad = " ".repeat(width - region.code.chars().count());
        text.push_str(&format!(
            "{}{pad}  {}\n",
            style.cyan(&region.code),
            region.name
        ));
    }
    let hint = format!(
        "--country {} alone for the whole country, or with --region and one of these.",
        country.code
    );
    text.push_str(&format!("\n{}\n", style.dim(&hint)));
    text
}
