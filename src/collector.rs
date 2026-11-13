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
use crate::notify::{Notice, Notifier};
use anyhow::Result;
use futures_util::StreamExt;
use futures_util::stream;
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior};
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
    notifier: Notifier,
}

impl Collector {
    pub fn new(pool: SqlitePool, settings: Settings) -> Result<Self> {
        Ok(Self {
            client: Client::new(&settings)?,
            pool,
            settings,
            notifier: Notifier::default(),
        })
    }

    /// The same collector, telling `notifier` what happens to the list.
    pub fn with_notifier(mut self, notifier: Notifier) -> Self {
        self.notifier = notifier;
        self
    }

    pub fn notifier(&self) -> &Notifier {
        &self.notifier
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
                    self.notifier.send(Notice::online(
                        &instance.url,
                        &instance.country,
                        instance.region.as_deref(),
                        &snapshot.version.version,
                        snapshot.nodes.len(),
                        now,
                    ));
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
        let days = self.settings.offline_after_days;
        let offline = db::mark_offline(&self.pool, offline_before, now).await?;
        for gone in &offline {
            info!("{} has not answered in {days} days: offline", gone.url);
            self.notifier.send(Notice::offline(
                &gone.url,
                &gone.country,
                gone.region.as_deref(),
                gone.last_error.as_deref(),
                days,
                now,
            ));
        }
        let days = self.settings.delete_after_days;
        let deleted = db::delete_offline(&self.pool, delete_before).await?;
        for gone in &deleted {
            info!("{} has been offline for {days} days: deleted", gone.url);
            self.notifier.send(Notice::deleted(
                &gone.url,
                &gone.country,
                gone.region.as_deref(),
                gone.last_error.as_deref(),
                days,
                now,
            ));
        }
        Ok(Cleanup {
            offline: offline.into_iter().map(|gone| gone.url).collect(),
            deleted: deleted.into_iter().map(|gone| gone.url).collect(),
        })
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

    /// Runs a round every interval, until told to stop. The first comes when
    /// the interval since the last one, as the database tells, is over: at
    /// once on a new database or after a long stop, later after a restart, so
    /// that restarts, even in a loop, do not read every instance each time. A
    /// round in progress when the stop comes is abandoned: each instance's
    /// transaction either committed or did not.
    pub async fn run(self, mut stopping: watch::Receiver<bool>) {
        let interval = self.settings.interval();
        let wait = match db::last_round(&self.pool).await {
            Ok(last) => first_wait(last, unix_now(), interval),
            Err(e) => {
                warn!("when was the last round: {e:#}");
                Duration::ZERO
            }
        };
        if !wait.is_zero() {
            info!(
                "last round was recent: next one in {} minutes",
                wait.as_secs().div_ceil(60)
            );
        }
        let mut ticks = tokio::time::interval_at(Instant::now() + wait, interval);
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

/// How long to wait for the first round, given when the last one was (unix
/// seconds, `None` without instances): what is left of the interval, nothing if
/// it is over, and never more than an interval, whatever the clock did.
pub fn first_wait(last_round: Option<i64>, now: i64, interval: Duration) -> Duration {
    let Some(last) = last_round else {
        return Duration::ZERO;
    };
    let interval = i64::try_from(interval.as_secs()).unwrap_or(i64::MAX);
    let left = last.saturating_add(interval).saturating_sub(now);
    Duration::from_secs(left.clamp(0, interval).unsigned_abs())
}

/// Now, in unix seconds.
pub fn unix_now() -> i64 {
    jiff::Timestamp::now().as_second()
}
