// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Seeding the list from a site that already lists Meshview instances, at its
//! `/api/instances`, in the shape of Meshview World's API.
//!
//! That list has a free text name for each instance; this one keeps an area
//! from a closed list. The instances known when this was written are mapped to
//! areas below, by URL. An instance listed that is not here is left out, with
//! a warning, to be submitted from the web like any other. Imported instances
//! are not probed: the collector reads them on its next round, and those that
//! never answer are cleaned up like any other.
//!
//! Their history comes too, from the `/api/histogram` beside the list: a count
//! of nodes for each instance and day. A day with no nodes counted was a day
//! the instance did not answer, so it is left out, a gap here as such a day
//! is. A day already here is kept as it is.

use crate::areas::Areas;
use crate::db;
use crate::net::{Client, check_url};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sqlx::SqlitePool;
use tracing::{info, warn};

/// The instances known on 2026-11-13: URL, country and region, with the name
/// they were listed under alongside.
pub const KNOWN: &[(&str, &str, Option<&str>)] = &[
    // San Francisco Bay Area
    ("https://meshview.bayme.sh", "US", Some("US-CA")),
    // Spain
    ("https://meshview.meshtastic.es", "ES", None),
    // New York City
    ("https://meshview.nyme.sh", "US", Some("US-NY")),
    // Hessen - Germany
    ("https://meshview.lsinfra.de", "DE", Some("DE-HE")),
    // North Georgia / East Tennessee - USA
    ("https://view.mtnme.sh", "US", Some("US-GA")),
    // Salt Lake City (USA)
    ("https://meshview.freq51.net", "US", Some("US-UT")),
    // Western Pennsylvania (USA)
    ("https://map.wpamesh.net", "US", Some("US-PA")),
    // Oregon (USA)
    ("https://meshview.meshoregon.com", "US", Some("US-OR")),
    // Pioneer Valley, Massachusetts (USA)
    ("https://meshview.pvmesh.org", "US", Some("US-MA")),
    // Georgia (USA)
    ("https://meshview.gamesh.net", "US", Some("US-GA")),
    // Croatia
    ("https://map.cromesh.eu", "HR", None),
    // Colombia
    ("https://meshview.meshcolombia.co", "CO", None),
    // Southern California
    ("https://meshview.socalmesh.org", "US", Some("US-CA")),
    // Louisiana (USA)
    ("https://meshview.louisianamesh.org", "US", Some("US-LA")),
    // Saint Louis (USA)
    ("https://meshview.meshstl.org", "US", Some("US-MO")),
    // Northeast Ohio
    ("https://meshview.neomesh.org", "US", Some("US-OH")),
    // Canada
    ("https://mv.canadaverse.org", "CA", None),
    // Northwest Indiana (USA)
    ("https://meshview.nwimesh.net", "US", Some("US-IN")),
    // Chicago (USA)
    ("https://meshview.chicagolandmesh.org", "US", Some("US-IL")),
    // Novosibirsk (Russia)
    ("https://meshview.meshmon.ru", "RU", Some("RU-NVS")),
    // Greece
    ("https://meshview.m-powered.gr", "GR", None),
    // Richmond VA (USA)
    ("https://meshview.rvame.sh", "US", Some("US-VA")),
    // Florida (USA)
    ("https://meshview.areyoumeshingwith.us", "US", Some("US-FL")),
    // South Carolina (USA)
    ("https://meshview.scmesh.us", "US", Some("US-SC")),
    // Kentucky (USA)
    ("https://view.mesh-ky.org", "US", Some("US-KY")),
    // Meshtastic Ile-De-Frabce
    ("https://meshview.retidurc.fr", "FR", Some("FR-IDF")),
    // Germany - Tecklenburger Land
    ("https://meshte.mooo.com", "DE", Some("DE-NW")),
    // Colorado
    (
        "https://map.meshtastic.coloradomesh.org",
        "US",
        Some("US-CO"),
    ),
    // Chile
    ("https://vista.meshchile.cl", "CL", None),
    // Virginia Beach (USA)
    ("https://meshview.vbmesh.net", "US", Some("US-VA")),
    // Puerto Rico
    ("https://meshview.prmsh.com", "PR", None),
    // Galiza
    (
        "https://meshview.mesh.comunidadeozulo.org",
        "ES",
        Some("ES-GA"),
    ),
    // California
    ("https://meshview.shastamesh.net", "US", Some("US-CA")),
    // Chicago - NW Suburbs
    ("https://meshview.bewbsconsulting.com", "US", Some("US-IL")),
    // Hungary
    ("https://mv868.czd.hu", "HU", None),
    // Greece Thessaly MeshView
    (
        "https://richards-farms-court-queue.trycloudflare.com",
        "GR",
        Some("GR-E"),
    ),
    // España
    ("https://mapa.meshtastic.es", "ES", None),
    // Indonesia
    ("https://meshview.nareswari.net", "ID", None),
    // Cape Town (ZA)
    ("https://meshview.zr1rf.za.net", "ZA", Some("ZA-WC")),
];

/// How far back the history is asked for: all of it.
const HISTORY_DAYS: u32 = 3650;

#[derive(Deserialize)]
struct InstancesBody {
    instances: Vec<OldInstance>,
}

#[derive(Deserialize)]
struct OldInstance {
    name: Option<String>,
    url: String,
}

#[derive(Deserialize)]
struct HistogramBody {
    instances: Vec<OldSeries>,
}

#[derive(Deserialize)]
struct OldSeries {
    url: String,
    points: Vec<OldPoint>,
}

#[derive(Deserialize)]
struct OldPoint {
    date: String,
    node_count: i64,
    recorded_at: Option<i64>,
}

/// What an import did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Imported {
    pub added: usize,
    pub already_there: usize,
    pub unknown: Vec<String>,
    /// Days of history added, over every instance.
    pub days: u64,
    /// Why the history could not be read, if it could not: the instances are
    /// in all the same.
    pub history_error: Option<String>,
}

/// Reads the list from `source` and adds the instances it knows an
/// area for.
pub async fn import(
    pool: &SqlitePool,
    client: &Client,
    source: &str,
    allow_private: bool,
    now: i64,
) -> Result<Imported> {
    let body = client
        .get(source)
        .await
        .with_context(|| format!("reading {source}"))?;
    let listed: InstancesBody =
        serde_json::from_slice(&body).with_context(|| format!("{source} is not the list"))?;
    if listed.instances.is_empty() {
        bail!("{source} lists no instances");
    }

    let mut imported = Imported::default();
    for instance in listed.instances {
        let url = match check_url(&instance.url, allow_private) {
            Ok(url) => url,
            Err(e) => {
                warn!("left out: {e:#}");
                imported.unknown.push(instance.url);
                continue;
            }
        };
        let Some((_, country, region)) = KNOWN.iter().find(|(known, ..)| *known == url) else {
            warn!(
                "left out {url} ({}): no area known for it; submit it from the web",
                instance.name.as_deref().unwrap_or("no name")
            );
            imported.unknown.push(url);
            continue;
        };
        match db::add_instance(pool, &url, country, *region, now).await? {
            Some(_) => {
                info!(
                    "added {url} in {}",
                    Areas::get().label(country, *region, crate::areas::Language::English)
                );
                imported.added += 1;
            }
            None => imported.already_there += 1,
        }
    }

    match history_source(source) {
        Some(history) => match import_history(pool, client, &history, allow_private, now).await {
            Ok(days) => imported.days = days,
            Err(e) => {
                warn!("the history was not imported: {e:#}");
                imported.history_error = Some(format!("{e:#}"));
            }
        },
        None => {
            imported.history_error = Some(format!(
                "{source} does not end in /api/instances, so where the history is is not known"
            ));
        }
    }
    Ok(imported)
}

/// Where the history is served, beside the list.
fn history_source(source: &str) -> Option<String> {
    let base = source
        .trim_end_matches('/')
        .strip_suffix("/api/instances")?;
    Some(format!("{base}/api/histogram?days={HISTORY_DAYS}"))
}

/// Adds the days of the history of the instances listed here.
async fn import_history(
    pool: &SqlitePool,
    client: &Client,
    source: &str,
    allow_private: bool,
    now: i64,
) -> Result<u64> {
    let body = client
        .get(source)
        .await
        .with_context(|| format!("reading {source}"))?;
    let history: HistogramBody =
        serde_json::from_slice(&body).with_context(|| format!("{source} is not the history"))?;
    let mut added = 0;
    for series in history.instances {
        let Ok(url) = check_url(&series.url, allow_private) else {
            continue;
        };
        let Some(instance) = db::instance_by_url(pool, &url).await? else {
            continue;
        };
        let days: Vec<(String, i64, i64)> = series
            .points
            .into_iter()
            .filter(|point| point.node_count > 0 && is_date(&point.date))
            .map(|point| {
                (
                    point.date,
                    point.node_count,
                    point.recorded_at.unwrap_or(now),
                )
            })
            .collect();
        let count = db::add_daily_counts(pool, instance.id, &days).await?;
        if count > 0 {
            info!("{count} days of history for {url}");
        }
        added += count;
    }
    Ok(added)
}

/// A date as the history keeps them, YYYY-MM-DD.
fn is_date(text: &str) -> bool {
    text.parse::<jiff::civil::Date>()
        .is_ok_and(|date| date.to_string() == text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_instance_is_in_a_real_area_and_a_normal_url() {
        let areas = Areas::get();
        for (url, country, region) in KNOWN {
            assert_eq!(
                areas.check(country, *region).unwrap(),
                (country.to_string(), region.map(str::to_string)),
                "{url}"
            );
            assert_eq!(check_url(url, false).unwrap(), *url);
        }
        let mut urls: Vec<_> = KNOWN.iter().map(|(url, ..)| url).collect();
        urls.sort();
        urls.dedup();
        assert_eq!(urls.len(), KNOWN.len(), "a URL is there twice");
    }

    #[test]
    fn the_history_is_beside_the_list() {
        assert_eq!(
            history_source("https://planet.example.org/api/instances").as_deref(),
            Some("https://planet.example.org/api/histogram?days=3650")
        );
        assert_eq!(
            history_source("http://127.0.0.1:9/api/instances/").as_deref(),
            Some("http://127.0.0.1:9/api/histogram?days=3650")
        );
        assert_eq!(history_source("https://example.org/list.json"), None);
    }

    #[test]
    fn dates_are_checked() {
        assert!(is_date("2026-03-05"));
        for bad in ["2026-3-5", "2026-02-30", "yesterday", "2026-03-05T00:00:00"] {
            assert!(!is_date(bad), "{bad}");
        }
    }
}
