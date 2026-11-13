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
