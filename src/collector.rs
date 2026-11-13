// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The collector: every few hours, read every instance, and forget those that
//! have stopped answering.
//!
//! Instances are read several at a time, each into its own transaction, so a
//! slow or broken one holds up nobody else. One that fails keeps what it had and
//! records why. One that has not answered for `offline_after_days` is marked
//! offline, which puts it last in the list and takes its nodes off the map, and
//! one offline for `delete_after_days` is deleted with all it had. Offline
//! instances are still read, and the first good answer brings one back.

use crate::config::Collector as Settings;
use crate::db::{self, Instance};
use crate::meshview;
use crate::net::Client;
use anyhow::Result;
use futures_util::StreamExt;
use futures_util::stream;
use sqlx::SqlitePool;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

const DAY: i64 = 86_400;

/// What a collection did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub succeeded: usize,
    pub failed: usize,
    pub nodes: usize,
}

/// What a cleanup did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cleanup {
    pub offline: Vec<String>,
    pub deleted: Vec<String>,
}

#[derive(Clone)]
pub struct Collector {
    pool: SqlitePool,
    client: Client,
    settings: Settings,
}

impl Collector {
    pub fn new(pool: SqlitePool, settings: Settings) -> Result<Self> {
        Ok(Self {
            client: Client::new(&settings)?,
            pool,
            settings,
        })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Reads every instance once.
    pub async fn collect_all(&self, now: i64) -> Result<Report> {
        let instances = db::instances(&self.pool).await?;
        let results: Vec<Option<usize>> = stream::iter(instances)
            .map(|instance| self.collect_one(instance, now))
            .buffer_unordered(self.settings.concurrency as usize)
            .collect()
            .await;

        let mut report = Report::default();
        for result in results {
            match result {
                Some(nodes) => {
                    report.succeeded += 1;
                    report.nodes += nodes;
                }
                None => report.failed += 1,
            }
        }
        Ok(report)
    }

    /// Reads one instance and records the outcome. Answers how many nodes it
    /// has, or `None` if it could not be read.
    async fn collect_one(&self, instance: Instance, now: i64) -> Option<usize> {
        let outcome = meshview::probe(&self.client, &instance.url, self.settings.days_active).await;
        let recorded = match &outcome {
            Ok(snapshot) => {
                if instance.offline_since.is_some() {
                    info!("{} answers again", instance.url);
                }
                db::record_success(
                    &self.pool,
                    instance.id,
                    now,
                    &snapshot.version,
                    &snapshot.nodes,
                )
                .await
            }
            Err(error) => {
                warn!("{}: {error}", instance.url);
                db::record_failure(&self.pool, instance.id, now, &error.to_string()).await
            }
        };
        if let Err(e) = recorded {
            warn!("{}: recording the collection: {e:#}", instance.url);
            return None;
        }
        outcome.ok().map(|snapshot| snapshot.nodes.len())
    }

    /// Marks offline the instances that have stopped answering, and deletes
    /// those offline for long enough.
    pub async fn cleanup(&self, now: i64) -> Result<Cleanup> {
        let offline_before = now - i64::from(self.settings.offline_after_days) * DAY;
        let delete_before = now - i64::from(self.settings.delete_after_days) * DAY;
        let offline = db::mark_offline(&self.pool, offline_before, now).await?;
        for url in &offline {
            info!(
                "{url} has not answered in {} days: offline",
                self.settings.offline_after_days
            );
        }
        let deleted = db::delete_offline(&self.pool, delete_before).await?;
        for url in &deleted {
            info!(
                "{url} has been offline for {} days: deleted",
                self.settings.delete_after_days
            );
        }
        Ok(Cleanup { offline, deleted })
    }

    /// One round: collect, then clean up.
    pub async fn round(&self, now: i64) -> Result<(Report, Cleanup)> {
        let report = self.collect_all(now).await?;
        info!(
            "collected {} instances ({} failed), {} nodes",
            report.succeeded, report.failed, report.nodes
        );
        let cleanup = self.cleanup(now).await?;
        Ok((report, cleanup))
    }

    /// Runs a round now and then every interval, until told to stop. A round in
    /// progress when the stop comes is abandoned: each instance's transaction
    /// either committed or did not.
    pub async fn run(self, mut stopping: watch::Receiver<bool>) {
        let mut ticks = tokio::time::interval(self.settings.interval());
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticks.tick() => {}
                _ = stopping.wait_for(|stop| *stop) => return,
            }
            tokio::select! {
                result = self.round(unix_now()) => {
                    if let Err(e) = result {
                        warn!("collection round: {e:#}");
                    }
                }
                _ = stopping.wait_for(|stop| *stop) => return,
            }
        }
    }
}

/// Now, in unix seconds.
pub fn unix_now() -> i64 {
    jiff::Timestamp::now().as_second()
}
