// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The database: one SQLite file, and every statement that touches it.
//!
//! The data is small (tens of instances, tens of thousands of nodes) and one
//! process both writes and reads it, so a single pool under WAL is all it
//! takes: readers do not wait for the collector, and the collector's writes,
//! one transaction per instance, are short.

use anyhow::{Context, Result};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{QueryBuilder, Row, SqlitePool};
use std::path::Path;
use std::time::Duration;
use tracing::warn;

/// The migrations, compiled into the binary.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// How long a statement waits for a lock before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(15);

const CONNECTIONS: u32 = 8;

/// Rows per INSERT when storing nodes: well under SQLite's limit of 32766
/// bound values, at 13 a node.
const NODES_PER_INSERT: usize = 1000;

/// Opens the database, creating it if it is not there, and brings its schema
/// up to date.
pub async fn open(path: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(BUSY_TIMEOUT)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        // The cascade from an instance to its nodes and history needs them.
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(CONNECTIONS)
        .connect_with(options)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    MIGRATOR
        .run(&pool)
        .await
        .with_context(|| format!("migrating {}", path.display()))?;
    Ok(pool)
}

/// Closes the pool, folding the write-ahead log back into the file first so
/// that nothing is left beside it.
pub async fn close(pool: SqlitePool) {
    if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await
    {
        warn!("could not fold the write-ahead log back into the database: {e}");
    }
    pool.close().await;
}

/// An instance, as the collector and the cleanup see it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Instance {
    pub id: i64,
    pub url: String,
    pub country: String,
    pub region: Option<String>,
    pub version: Option<String>,
    pub git_revision: Option<String>,
    pub git_revision_short: Option<String>,
    pub added_at: i64,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub last_error: Option<String>,
    pub offline_since: Option<i64>,
}

/// What an instance's /version says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Version {
    pub version: String,
    pub git_revision: Option<String>,
    pub git_revision_short: Option<String>,
}

/// A node, as an instance reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub node_id: i64,
    pub long_name: Option<String>,
    pub short_name: Option<String>,
    pub hw_model: Option<String>,
    pub firmware: Option<String>,
    pub role: Option<String>,
    pub channel: Option<String>,
    /// Degrees, both or neither.
    pub position: Option<(f64, f64)>,
    pub is_mqtt_gateway: Option<bool>,
    pub first_seen_us: Option<i64>,
    pub last_seen_us: Option<i64>,
}

/// Adds an instance. Answers `None`, adding nothing, when its URL is already
/// there.
pub async fn add_instance(
    pool: &SqlitePool,
    url: &str,
    country: &str,
    region: Option<&str>,
    now: i64,
) -> Result<Option<i64>> {
    let id = sqlx::query_scalar(
        "INSERT INTO instance (url, country, region, added_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (url) DO NOTHING
         RETURNING id",
    )
    .bind(url)
    .bind(country)
    .bind(region)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

pub async fn instance_by_url(pool: &SqlitePool, url: &str) -> Result<Option<Instance>> {
    Ok(sqlx::query_as("SELECT * FROM instance WHERE url = ?")
        .bind(url)
        .fetch_optional(pool)
        .await?)
}

/// Every instance, in the order they were added.
pub async fn instances(pool: &SqlitePool) -> Result<Vec<Instance>> {
    Ok(sqlx::query_as("SELECT * FROM instance ORDER BY id")
        .fetch_all(pool)
        .await?)
}

/// Records a good collection: what the instance runs, its nodes in place of
/// those it had, and today's count. All or nothing.
pub async fn record_success(
    pool: &SqlitePool,
    instance: i64,
    now: i64,
    version: &Version,
    nodes: &[Node],
) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "UPDATE instance SET version = ?, git_revision = ?, git_revision_short = ?,
             last_attempt_at = ?, last_success_at = ?, last_error = NULL, offline_since = NULL
         WHERE id = ?",
    )
    .bind(&version.version)
    .bind(&version.git_revision)
    .bind(&version.git_revision_short)
    .bind(now)
    .bind(now)
    .bind(instance)
    .execute(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM node WHERE instance_id = ?")
        .bind(instance)
        .execute(&mut *tx)
        .await?;

    for chunk in nodes.chunks(NODES_PER_INSERT) {
        let mut insert = QueryBuilder::new(
            "INSERT OR REPLACE INTO node (instance_id, node_id, long_name, short_name, hw_model,
                 firmware, role, channel, lat, lon, is_mqtt_gateway, first_seen_us,
                 last_seen_us) ",
        );
        insert.push_values(chunk, |mut row, node| {
            row.push_bind(instance)
                .push_bind(node.node_id)
                .push_bind(&node.long_name)
                .push_bind(&node.short_name)
                .push_bind(&node.hw_model)
                .push_bind(&node.firmware)
                .push_bind(&node.role)
                .push_bind(&node.channel)
                .push_bind(node.position.map(|(lat, _)| lat))
                .push_bind(node.position.map(|(_, lon)| lon))
                .push_bind(node.is_mqtt_gateway)
                .push_bind(node.first_seen_us)
                .push_bind(node.last_seen_us);
        });
        insert.build().execute(&mut *tx).await?;
    }

    sqlx::query(
        "INSERT INTO instance_daily (instance_id, date, node_count, recorded_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (instance_id, date)
         DO UPDATE SET node_count = excluded.node_count, recorded_at = excluded.recorded_at",
    )
    .bind(instance)
    .bind(utc_date(now))
    .bind(nodes.len() as i64)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Records a failed collection, leaving everything it had as it was.
pub async fn record_failure(pool: &SqlitePool, instance: i64, now: i64, error: &str) -> Result<()> {
    sqlx::query("UPDATE instance SET last_attempt_at = ?, last_error = ? WHERE id = ?")
        .bind(now)
        .bind(error)
        .bind(instance)
        .execute(pool)
        .await?;
    Ok(())
}

/// Marks offline the instances that have not had a good collection since
/// `before` (counting from when they were added, for one that never had one).
/// Answers their URLs.
pub async fn mark_offline(pool: &SqlitePool, before: i64, now: i64) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "UPDATE instance SET offline_since = ?
         WHERE offline_since IS NULL AND COALESCE(last_success_at, added_at) < ?
         RETURNING url",
    )
    .bind(now)
    .bind(before)
    .fetch_all(pool)
    .await?)
}

/// Deletes the instances offline since before `before`, with their nodes and
/// their history. Answers their URLs.
pub async fn delete_offline(pool: &SqlitePool, before: i64) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "DELETE FROM instance WHERE offline_since IS NOT NULL AND offline_since < ?
         RETURNING url",
    )
    .bind(before)
    .fetch_all(pool)
    .await?)
}

/// How many nodes an instance has stored.
pub async fn count_nodes(pool: &SqlitePool, instance: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM node WHERE instance_id = ?")
            .bind(instance)
            .fetch_one(pool)
            .await?,
    )
}

/// An instance's daily counts, oldest first, as (date, count).
pub async fn daily_counts(pool: &SqlitePool, instance: i64) -> Result<Vec<(String, i64)>> {
    let rows = sqlx::query(
        "SELECT date, node_count FROM instance_daily WHERE instance_id = ? ORDER BY date",
    )
    .bind(instance)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get("date"), row.get("node_count")))
        .collect())
}

/// A fingerprint of everything the web answers from: it changes whenever an
/// instance is added, collected (well or not), marked offline or deleted, and
/// nothing the web shows changes without one of those. One pass over a table
/// of tens of rows, so cheap enough to ask before every cached answer, and
/// right even when another process (an import) wrote.
pub async fn fingerprint(pool: &SqlitePool) -> Result<String> {
    let row = sqlx::query(
        "SELECT COUNT(*), COALESCE(MAX(id), 0), COALESCE(SUM(last_attempt_at), 0),
             COALESCE(SUM(offline_since), 0)
         FROM instance",
    )
    .fetch_one(pool)
    .await?;
    Ok(format!(
        "{}-{}-{}-{}",
        row.get::<i64, _>(0),
        row.get::<i64, _>(1),
        row.get::<i64, _>(2),
        row.get::<i64, _>(3)
    ))
}

/// Which nodes to read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NodeFilter {
    /// Only those seen since, in microseconds.
    pub seen_since_us: i64,
    pub country: Option<String>,
    pub region: Option<String>,
    pub channel: Option<String>,
    pub instance_url: Option<String>,
}

/// A node with the instance it comes from.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct NodeRow {
    pub instance_url: String,
    pub country: String,
    pub region: Option<String>,
    pub node_id: i64,
    pub long_name: Option<String>,
    pub short_name: Option<String>,
    pub hw_model: Option<String>,
    pub firmware: Option<String>,
    pub role: Option<String>,
    pub channel: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub is_mqtt_gateway: Option<bool>,
    pub first_seen_us: Option<i64>,
    pub last_seen_us: Option<i64>,
}

/// The nodes of the instances that are online, with or without a position.
/// One pass over the nodes, in the order of their key: a node seen recently
/// enough is most of them, so walking an index on when it was seen would only
/// add lookups.
pub async fn nodes(pool: &SqlitePool, filter: &NodeFilter) -> Result<Vec<NodeRow>> {
    let mut query = QueryBuilder::new(
        "SELECT i.url AS instance_url, i.country, i.region, n.node_id, n.long_name,
             n.short_name, n.hw_model, n.firmware, n.role, n.channel, n.lat, n.lon,
             n.is_mqtt_gateway, n.first_seen_us, n.last_seen_us
         FROM node n JOIN instance i ON i.id = n.instance_id
         WHERE i.offline_since IS NULL AND n.last_seen_us >= ",
    );
    query.push_bind(filter.seen_since_us);
    if let Some(country) = &filter.country {
        query.push(" AND i.country = ").push_bind(country);
    }
    if let Some(region) = &filter.region {
        query.push(" AND i.region = ").push_bind(region);
    }
    if let Some(channel) = &filter.channel {
        query.push(" AND n.channel = ").push_bind(channel);
    }
    if let Some(url) = &filter.instance_url {
        query.push(" AND i.url = ").push_bind(url);
    }
    query.push(" ORDER BY n.instance_id, n.node_id");
    Ok(query.build_query_as().fetch_all(pool).await?)
}

/// An instance with its figures, for the list.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct InstanceSummary {
    #[sqlx(flatten)]
    pub instance: Instance,
    pub node_count: i64,
    pub mapped_count: i64,
    /// The first and the latest daily count since the trend's start, if any.
    pub trend_start: Option<i64>,
    pub trend_end: Option<i64>,
}

/// Every instance with its node counts and its trend since `trend_since` (a
/// date): online first, then by nodes. The counts come from one grouped pass
/// over the nodes, and the trend from the history's key, two rows each.
pub async fn instance_summaries(
    pool: &SqlitePool,
    trend_since: &str,
) -> Result<Vec<InstanceSummary>> {
    Ok(sqlx::query_as(
        "SELECT i.*,
             COALESCE(n.node_count, 0) AS node_count,
             COALESCE(n.mapped_count, 0) AS mapped_count,
             (SELECT d.node_count FROM instance_daily d
              WHERE d.instance_id = i.id AND d.date >= ?1
              ORDER BY d.date LIMIT 1) AS trend_start,
             (SELECT d.node_count FROM instance_daily d
              WHERE d.instance_id = i.id AND d.date >= ?1
              ORDER BY d.date DESC LIMIT 1) AS trend_end
         FROM instance i
         LEFT JOIN (
             SELECT instance_id, COUNT(*) AS node_count, COUNT(lat) AS mapped_count
             FROM node GROUP BY instance_id
         ) n ON n.instance_id = i.id
         ORDER BY i.offline_since IS NOT NULL, node_count DESC, i.id",
    )
    .bind(trend_since)
    .fetch_all(pool)
    .await?)
}

/// A daily count.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DailyRow {
    pub instance_id: i64,
    pub date: String,
    pub node_count: i64,
    pub recorded_at: i64,
}

/// The daily counts between two dates, both included, by instance and date:
/// the history's key order, so read without sorting.
pub async fn daily_between(pool: &SqlitePool, from: &str, to: &str) -> Result<Vec<DailyRow>> {
    Ok(sqlx::query_as(
        "SELECT instance_id, date, node_count, recorded_at FROM instance_daily
         WHERE date >= ? AND date <= ?
         ORDER BY instance_id, date",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?)
}

/// The UTC date `days` days before a moment's, as YYYY-MM-DD.
pub fn utc_date_days_before(unix_seconds: i64, days: i64) -> String {
    utc_date(unix_seconds - days * 86_400)
}

/// The UTC date of a moment in unix seconds, as YYYY-MM-DD.
pub fn utc_date(unix_seconds: i64) -> String {
    jiff::Timestamp::from_second(unix_seconds)
        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::UTC).date().to_string())
        .unwrap_or_else(|_| "1970-01-01".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc() {
        assert_eq!(utc_date(0), "1970-01-01");
        // 2026-11-13 23:59:59 UTC, and a second later.
        assert_eq!(utc_date(1_794_614_399), "2026-11-13");
        assert_eq!(utc_date(1_794_614_400), "2026-11-14");
    }
}
