// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The collector and the cleanup, against pretend instances.

mod common;

use common::{Instance, Kind, planet, settings};
use meshview_planet::collector::Report;
use meshview_planet::db;

const DAY: i64 = 86_400;
/// 2026-11-13 12:00 UTC.
const NOW: i64 = 1_794_571_200;

#[tokio::test]
async fn every_kind_of_meshview_is_read() {
    let rust = Instance::start(Kind::Rust, 10).await;
    let python = Instance::start(Kind::Python, 6).await;
    let plus = Instance::start(Kind::Plus, 3).await;
    let (_dir, pool, collector) = planet(settings()).await;
    for instance in [&rust, &python, &plus] {
        db::add_instance(&pool, &instance.url, "ES", Some("ES-GA"), NOW)
            .await
            .unwrap();
    }

    let report = collector.collect_all(NOW).await.unwrap();
    assert_eq!(
        report,
        Report {
            succeeded: 3,
            failed: 0,
            nodes: 19
        }
    );

    let stored = db::instances(&pool).await.unwrap();
    assert_eq!(stored[0].version.as_deref(), Some("5.0.0"));
    assert_eq!(stored[0].git_revision_short.as_deref(), Some("21714da"));
    assert_eq!(stored[0].flavor.as_deref(), Some("rust"));
    assert_eq!(stored[1].flavor, None);
    assert_eq!(stored[1].version.as_deref(), Some("4.0.0"));
    assert_eq!(stored[2].version.as_deref(), Some("plus 1.0.0"));
    assert_eq!(stored[2].git_revision, None);
    for (instance, nodes) in stored.iter().zip([10, 6, 3]) {
        assert_eq!(instance.last_success_at, Some(NOW));
        assert_eq!(instance.last_error, None);
        assert_eq!(db::count_nodes(&pool, instance.id).await.unwrap(), nodes);
        assert_eq!(
            db::daily_counts(&pool, instance.id).await.unwrap(),
            [("2026-11-13".to_string(), nodes)]
        );
    }
}

#[tokio::test]
async fn nodes_are_replaced_and_the_day_s_count_is_the_latest() {
    let instance = Instance::start(Kind::Rust, 10).await;
    let (_dir, pool, collector) = planet(settings()).await;
    let id = db::add_instance(&pool, &instance.url, "ES", None, NOW)
        .await
        .unwrap()
        .unwrap();

    collector.collect_all(NOW).await.unwrap();
    instance.set_nodes(4);
    collector.collect_all(NOW + 3600).await.unwrap();
    assert_eq!(db::count_nodes(&pool, id).await.unwrap(), 4);
    instance.set_nodes(7);
    collector.collect_all(NOW + DAY).await.unwrap();

    assert_eq!(
        db::daily_counts(&pool, id).await.unwrap(),
        [("2026-11-13".to_string(), 4), ("2026-11-14".to_string(), 7)]
    );
}

#[tokio::test]
async fn a_failing_instance_keeps_what_it_had() {
    let instance = Instance::start(Kind::Python, 5).await;
    let (_dir, pool, collector) = planet(settings()).await;
    let id = db::add_instance(&pool, &instance.url, "ES", None, NOW)
        .await
        .unwrap()
        .unwrap();
    collector.collect_all(NOW).await.unwrap();

    for (kind, error) in [
        (Kind::Broken, "/version: answered HTTP 500"),
        (Kind::NotMeshview, "does not answer like Meshview"),
        (Kind::Huge, "answer larger than 1 MB"),
    ] {
        instance.set(kind);
        let report = collector.collect_all(NOW + 60).await.unwrap();
        assert_eq!(report.failed, 1, "{kind:?}");
        let stored = db::instance_by_url(&pool, &instance.url)
            .await
            .unwrap()
            .unwrap();
        assert!(
            stored.last_error.as_deref().unwrap().contains(error),
            "{kind:?}: {:?}",
            stored.last_error
        );
        assert_eq!(stored.last_attempt_at, Some(NOW + 60));
        assert_eq!(stored.last_success_at, Some(NOW));
        assert_eq!(db::count_nodes(&pool, id).await.unwrap(), 5);
    }
}

#[tokio::test]
async fn an_unreachable_instance_is_a_failure_not_an_error() {
    // A port nothing listens on: bound, then let go.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let (_dir, pool, collector) = planet(settings()).await;
    db::add_instance(&pool, &url, "ES", None, NOW)
        .await
        .unwrap();

    let report = collector.collect_all(NOW).await.unwrap();
    assert_eq!(report.failed, 1);
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert!(stored.last_error.is_some());
}

#[tokio::test]
async fn silent_instances_go_offline_come_back_and_are_deleted() {
    let instance = Instance::start(Kind::Rust, 5).await;
    let (_dir, pool, collector) = planet(settings()).await;
    let id = db::add_instance(&pool, &instance.url, "ES", None, NOW)
        .await
        .unwrap()
        .unwrap();
    collector.collect_all(NOW).await.unwrap();
    instance.set(Kind::Broken);

    // Three days without an answer is not yet more than three days.
    let (_, cleanup) = collector.round(NOW + 3 * DAY).await.unwrap();
    assert!(cleanup.offline.is_empty());

    let later = NOW + 3 * DAY + 1;
    let (_, cleanup) = collector.round(later).await.unwrap();
    assert_eq!(cleanup.offline, std::slice::from_ref(&instance.url));
    let stored = db::instance_by_url(&pool, &instance.url)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.offline_since, Some(later));
    // Its nodes are kept, for when it comes back.
    assert_eq!(db::count_nodes(&pool, id).await.unwrap(), 5);

    // It comes back.
    instance.set(Kind::Rust);
    let (_, cleanup) = collector.round(later + DAY).await.unwrap();
    assert!(cleanup.offline.is_empty() && cleanup.deleted.is_empty());
    let stored = db::instance_by_url(&pool, &instance.url)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.offline_since, None);

    // And goes away for good.
    instance.set(Kind::Broken);
    let gone_since = later + DAY;
    let offline_at = gone_since + 3 * DAY + 1;
    let (_, cleanup) = collector.round(offline_at).await.unwrap();
    assert_eq!(cleanup.offline.len(), 1);
    let (_, cleanup) = collector.round(offline_at + 30 * DAY).await.unwrap();
    assert!(cleanup.deleted.is_empty());
    let (_, cleanup) = collector.round(offline_at + 30 * DAY + 1).await.unwrap();
    assert_eq!(cleanup.deleted, std::slice::from_ref(&instance.url));

    // With everything it had.
    assert!(db::instances(&pool).await.unwrap().is_empty());
    assert_eq!(db::count_nodes(&pool, id).await.unwrap(), 0);
    assert!(db::daily_counts(&pool, id).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_instance_that_never_answered_goes_offline_from_when_it_was_added() {
    let instance = Instance::start(Kind::Broken, 0).await;
    let (_dir, pool, collector) = planet(settings()).await;
    db::add_instance(&pool, &instance.url, "ES", None, NOW)
        .await
        .unwrap();

    let (_, cleanup) = collector.round(NOW + DAY).await.unwrap();
    assert!(cleanup.offline.is_empty());
    let (_, cleanup) = collector.round(NOW + 3 * DAY + 1).await.unwrap();
    assert_eq!(cleanup.offline.len(), 1);
}

#[tokio::test]
async fn the_periods_are_the_configured_ones() {
    let instance = Instance::start(Kind::Broken, 0).await;
    let (_dir, pool, collector) = planet(meshview_planet::config::Collector {
        offline_after_days: 1,
        delete_after_days: 2,
        ..settings()
    })
    .await;
    db::add_instance(&pool, &instance.url, "ES", None, NOW)
        .await
        .unwrap();

    let (_, cleanup) = collector.round(NOW + DAY + 1).await.unwrap();
    assert_eq!(cleanup.offline.len(), 1);
    let (_, cleanup) = collector.round(NOW + 3 * DAY + 2).await.unwrap();
    assert_eq!(cleanup.deleted.len(), 1);
}

#[test]
fn the_first_round_waits_for_what_is_left_of_the_interval() {
    use meshview_planet::collector::first_wait;
    use std::time::Duration;
    let hour = Duration::from_secs(3600);
    // Nothing to read, or never read: now.
    assert_eq!(first_wait(None, NOW, hour), Duration::ZERO);
    assert_eq!(first_wait(Some(0), NOW, hour), Duration::ZERO);
    // Ten minutes ago: fifty more.
    assert_eq!(
        first_wait(Some(NOW - 600), NOW, hour),
        Duration::from_secs(3000)
    );
    // Long ago: now.
    assert_eq!(first_wait(Some(NOW - 7200), NOW, hour), Duration::ZERO);
    // In the future (the clock went back): one interval, no more.
    assert_eq!(first_wait(Some(NOW + 86_400), NOW, hour), hour);
}

#[tokio::test]
async fn the_last_round_is_the_oldest_attempt() {
    let (_dir, pool, collector) = planet(settings()).await;
    assert_eq!(db::last_round(&pool).await.unwrap(), None);
    let first = Instance::start(Kind::Rust, 3).await;
    db::add_instance(&pool, &first.url, "ES", None, NOW)
        .await
        .unwrap();
    // Never read: a round is due.
    assert_eq!(db::last_round(&pool).await.unwrap(), Some(0));
    collector.round(NOW).await.unwrap();
    assert_eq!(db::last_round(&pool).await.unwrap(), Some(NOW));
    // One added from the web later, and read then, does not move the round.
    let id = db::add_instance(&pool, "https://later.example.org", "PT", None, NOW + 600)
        .await
        .unwrap()
        .unwrap();
    db::record_success(&pool, id, NOW + 600, &db::Version::default(), &[])
        .await
        .unwrap();
    assert_eq!(db::last_round(&pool).await.unwrap(), Some(NOW));
}

/// Runs the collector for a moment, as if the service had just started.
async fn start_for_a_moment(collector: meshview_planet::collector::Collector) {
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(collector.run(stopping));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    stop.send(true).unwrap();
    running.await.unwrap();
}

#[tokio::test]
async fn a_restart_does_not_read_every_instance_again() {
    let (_dir, pool, collector) = planet(settings()).await;
    let instance = Instance::start(Kind::Rust, 3).await;
    let now = meshview_planet::collector::unix_now();
    db::add_instance(&pool, &instance.url, "ES", None, now)
        .await
        .unwrap();
    // A round ten minutes ago, of the four hours.
    collector.round(now - 600).await.unwrap();
    start_for_a_moment(collector.clone()).await;
    assert_eq!(db::last_round(&pool).await.unwrap(), Some(now - 600));

    // Five hours ago instead: due, so at once.
    collector.round(now - 5 * 3600).await.unwrap();
    start_for_a_moment(collector).await;
    assert!(db::last_round(&pool).await.unwrap().unwrap() >= now);
}
