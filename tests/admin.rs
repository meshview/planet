// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! What the admin does from the command line, against pretend instances.

mod common;

use common::{Instance, Kind, planet, settings};
use meshview_planet::admin::{self, Added};
use meshview_planet::db;

/// 2026-11-13 12:00 UTC.
const NOW: i64 = 1_794_571_200;
const DAY: i64 = 86_400;

#[tokio::test]
async fn an_instance_that_answers_is_added_with_its_nodes() {
    let (_dir, pool, collector) = planet(settings()).await;
    let instance = Instance::start(Kind::Rust, 10).await;
    let (url, added) = admin::add(
        &pool,
        &collector,
        &format!("{}/", instance.url),
        "es",
        Some("es-ga"),
        false,
        NOW,
    )
    .await
    .unwrap();
    assert_eq!(url, instance.url);
    assert_eq!(
        added,
        Added::Answered {
            version: "5.0.0".into(),
            nodes: 10
        }
    );
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(stored.region.as_deref(), Some("ES-GA"));
    assert_eq!(db::count_nodes(&pool, stored.id).await.unwrap(), 10);
    assert_eq!(admin::state(&stored), "online");

    let twice = admin::add(&pool, &collector, &url, "ES", None, false, NOW).await;
    assert!(twice.unwrap_err().to_string().contains("already listed"));
}

#[tokio::test]
async fn one_that_does_not_answer_needs_force() {
    let (_dir, pool, collector) = planet(settings()).await;
    let instance = Instance::start(Kind::Broken, 3).await;
    let refused = admin::add(&pool, &collector, &instance.url, "ES", None, false, NOW)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("--force"), "{refused}");
    assert!(db::instances(&pool).await.unwrap().is_empty());

    // Nor is the area ever free text, forced or not.
    let bad_area = admin::add(&pool, &collector, &instance.url, "XX", None, true, NOW).await;
    assert!(bad_area.is_err());

    let (url, added) = admin::add(&pool, &collector, &instance.url, "ES", None, true, NOW)
        .await
        .unwrap();
    assert_eq!(
        added,
        Added::NotAnswering("/version: answered HTTP 500".into())
    );
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(admin::state(&stored), "failing");

    // Read by the rounds: when it answers, it is in.
    instance.set(Kind::Rust);
    collector.round(NOW + 3600).await.unwrap();
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(admin::state(&stored), "online");
    assert_eq!(db::count_nodes(&pool, stored.id).await.unwrap(), 3);
}

#[tokio::test]
async fn removing_deletes_everything() {
    let (_dir, pool, collector) = planet(settings()).await;
    let instance = Instance::start(Kind::Rust, 4).await;
    admin::add(&pool, &collector, &instance.url, "ES", None, false, NOW)
        .await
        .unwrap();
    // Written as it was added or not: the slash does not matter.
    let url = admin::remove(&pool, &format!("{}/", instance.url))
        .await
        .unwrap();
    assert_eq!(url, instance.url);
    assert!(db::instances(&pool).await.unwrap().is_empty());
    assert!(db::daily_counts(&pool, 1).await.unwrap().is_empty());
    assert!(admin::remove(&pool, &url).await.is_err());
}

#[tokio::test]
async fn disabling_keeps_it_and_enabling_reads_it_again() {
    let (_dir, pool, collector) = planet(settings()).await;
    let instance = Instance::start(Kind::Rust, 4).await;
    let (url, _) = admin::add(&pool, &collector, &instance.url, "ES", None, false, NOW)
        .await
        .unwrap();

    // Offline when disabled.
    instance.set(Kind::Broken);
    collector.round(NOW + 4 * DAY).await.unwrap();
    admin::disable(&pool, &url, NOW + 4 * DAY).await.unwrap();
    assert!(admin::disable(&pool, &url, NOW).await.is_err());
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(admin::state(&stored), "disabled");
    assert_eq!(db::instances(&pool).await.unwrap().len(), 1);
    assert!(db::enabled_instances(&pool).await.unwrap().is_empty());
    // Disabled, it is not even counted for when the last round was.
    assert_eq!(db::last_round(&pool).await.unwrap(), None);
    let twice = admin::add(&pool, &collector, &url, "ES", None, true, NOW).await;
    assert!(twice.unwrap_err().to_string().contains("enable it"));

    // Long after, enabled again while it still fails: back in, and not
    // deleted by the next cleanup for having been offline so long.
    let later = NOW + 90 * DAY;
    let enabled = admin::enable(&pool, &collector, &url, later).await.unwrap();
    assert_eq!(enabled.disabled_at, None);
    assert_eq!(enabled.offline_since, None);
    assert_eq!(enabled.last_attempt_at, Some(later));
    assert!(enabled.last_error.is_some());
    collector.cleanup(later).await.unwrap();
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(stored.offline_since, Some(later));
    assert!(admin::enable(&pool, &collector, &url, later).await.is_err());

    // And when it answers, online.
    instance.set(Kind::Rust);
    collector.round(later + 3600).await.unwrap();
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(admin::state(&stored), "online");
}

#[test]
fn the_areas_to_choose_from() {
    let all = admin::areas(None).unwrap();
    assert!(all.lines().count() > 200);
    assert!(all.contains("ES  Spain\n"));

    // By code, in any case, or by its English name: its regions.
    for query in ["ES", "es", "spain", "Spa"] {
        let spain = admin::areas(Some(query)).unwrap();
        assert!(spain.starts_with("ES  Spain\n"), "{query}: {spain}");
        assert!(spain.contains("ES-GA  Galicia\n"), "{query}");
        assert!(spain.contains("--country ES"), "{query}");
    }
    // A name that fits several: the countries.
    let guineas = admin::areas(Some("guinea")).unwrap();
    assert!(guineas.contains("GN  Guinea\n"));
    assert!(guineas.contains("PG  Papua New Guinea"));
    assert!(!guineas.contains("--country"));
    assert!(admin::areas(Some("narnia")).is_err());
    // English only, as the rest of the command line.
    assert!(admin::areas(Some("españa")).is_err());
}
