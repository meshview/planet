// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! What the admin does from the command line, against pretend instances.

mod common;

use common::{Instance, Kind, planet, settings};
use meshview_planet::admin::{self, Added, State};
use meshview_planet::db;
use meshview_planet::term::Style;

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
        // The region names the country.
        None,
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
    assert_eq!(admin::state(&stored), State::Online);

    let twice = admin::add(&pool, &collector, &url, Some("ES"), None, false, NOW).await;
    assert!(twice.unwrap_err().to_string().contains("already listed"));
}

#[tokio::test]
async fn one_that_does_not_answer_needs_force() {
    let (_dir, pool, collector) = planet(settings()).await;
    let instance = Instance::start(Kind::Broken, 3).await;
    let refused = admin::add(
        &pool,
        &collector,
        &instance.url,
        Some("ES"),
        None,
        false,
        NOW,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(refused.contains("--force"), "{refused}");
    assert!(db::instances(&pool).await.unwrap().is_empty());

    // Nor is the area ever free text, forced or not.
    let bad_area = admin::add(
        &pool,
        &collector,
        &instance.url,
        Some("XX"),
        None,
        true,
        NOW,
    )
    .await;
    assert!(bad_area.is_err());

    let (url, added) = admin::add(
        &pool,
        &collector,
        &instance.url,
        Some("ES"),
        None,
        true,
        NOW,
    )
    .await
    .unwrap();
    assert_eq!(
        added,
        Added::NotAnswering("/version: answered HTTP 500".into())
    );
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(admin::state(&stored), State::Failing);

    // Read by the rounds: when it answers, it is in.
    instance.set(Kind::Rust);
    collector.round(NOW + 3600).await.unwrap();
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(admin::state(&stored), State::Online);
    assert_eq!(db::count_nodes(&pool, stored.id).await.unwrap(), 3);
}

#[tokio::test]
async fn removing_deletes_everything() {
    let (_dir, pool, collector) = planet(settings()).await;
    let instance = Instance::start(Kind::Rust, 4).await;
    admin::add(
        &pool,
        &collector,
        &instance.url,
        Some("ES"),
        None,
        false,
        NOW,
    )
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
    let (url, _) = admin::add(
        &pool,
        &collector,
        &instance.url,
        Some("ES"),
        None,
        false,
        NOW,
    )
    .await
    .unwrap();

    // Offline when disabled.
    instance.set(Kind::Broken);
    collector.round(NOW + 4 * DAY).await.unwrap();
    admin::disable(&pool, &url, NOW + 4 * DAY).await.unwrap();
    assert!(admin::disable(&pool, &url, NOW).await.is_err());
    let stored = db::instance_by_url(&pool, &url).await.unwrap().unwrap();
    assert_eq!(admin::state(&stored), State::Disabled);
    assert_eq!(db::instances(&pool).await.unwrap().len(), 1);
    assert!(db::enabled_instances(&pool).await.unwrap().is_empty());
    // Disabled, it is not even counted for when the last round was.
    assert_eq!(db::last_round(&pool).await.unwrap(), None);
    let twice = admin::add(&pool, &collector, &url, Some("ES"), None, true, NOW).await;
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
    assert_eq!(admin::state(&stored), State::Online);
}

#[test]
fn the_areas_to_choose_from() {
    let all = admin::areas(None, Style::plain()).unwrap();
    assert!(all.lines().count() > 200);
    assert!(all.contains("ES  Spain\n"));

    // By code, in any case, or by its English name: its regions.
    for query in ["ES", "es", "spain", "Spa"] {
        let spain = admin::areas(Some(query), Style::plain()).unwrap();
        assert!(spain.starts_with("ES  Spain\n"), "{query}: {spain}");
        assert!(spain.contains("ES-GA  Galicia\n"), "{query}");
        assert!(spain.contains("--country ES"), "{query}");
    }
    // A name that fits several: the countries.
    let guineas = admin::areas(Some("guinea"), Style::plain()).unwrap();
    assert!(guineas.contains("GN  Guinea\n"));
    assert!(guineas.contains("PG  Papua New Guinea"));
    assert!(!guineas.contains("--country"));
    assert!(admin::areas(Some("narnia"), Style::plain()).is_err());
    // English only, as the rest of the command line.
    assert!(admin::areas(Some("españa"), Style::plain()).is_err());
}

#[test]
fn the_list_is_grouped_by_state() {
    let instance = |id: i64, url: &str| db::Instance {
        id,
        url: url.into(),
        country: "ES".into(),
        region: Some("ES-GA".into()),
        version: Some("5.0.0".into()),
        git_revision: None,
        git_revision_short: None,
        flavor: None,
        added_at: NOW - 40 * DAY,
        last_attempt_at: Some(NOW - 600),
        last_success_at: Some(NOW - 600),
        last_error: None,
        offline_since: None,
        disabled_at: None,
    };
    let small = instance(1, "https://small.example.org");
    let big = instance(2, "https://big.example.org");
    let failing = db::Instance {
        last_success_at: Some(NOW - 2 * 3600),
        last_error: Some("/version: answered HTTP 500".into()),
        ..instance(3, "https://failing.example.org")
    };
    let offline = db::Instance {
        last_success_at: None,
        last_error: Some("/version: did not answer in time".into()),
        offline_since: Some(NOW - 10 * DAY),
        ..instance(4, "https://offline.example.org")
    };
    let disabled = db::Instance {
        disabled_at: Some(NOW - 3 * DAY),
        ..instance(5, "https://disabled.example.org")
    };
    let listed = vec![
        (disabled, 7),
        (offline, 0),
        (small, 3),
        (failing, 12),
        (big, 900),
    ];
    let text = admin::instance_list(listed, NOW, &settings(), Style::plain());

    // Online first, by nodes; then failing, offline and disabled.
    let order: Vec<usize> = [
        "● Online (2)",
        "https://big.example.org",
        "https://small.example.org",
        "● Failing (1)",
        "https://failing.example.org",
        "● Offline (1)",
        "https://offline.example.org",
        "● Disabled (1)",
        "https://disabled.example.org",
    ]
    .iter()
    .map(|wanted| {
        text.find(wanted)
            .unwrap_or_else(|| panic!("{wanted}:\n{text}"))
    })
    .collect();
    assert!(order.is_sorted(), "{text}");

    // Two hours silent of the three days: offline in three.
    assert!(text.contains("2 h ago      in 3 days  /version"), "{text}");
    assert!(text.contains("/version: answered HTTP 500"));
    assert!(
        text.contains("10 days ago  in 20 days  /version: did not answer in time"),
        "{text}"
    );
    assert!(text.contains("3 days ago"));
    assert!(text.ends_with("5 instances: 2 online, 1 failing, 1 offline, 1 disabled\n"));
    // The common columns line up across the groups.
    let column = |url: &str| {
        let line = text.lines().find(|line| line.contains(url)).unwrap();
        line.find(url).unwrap()
    };
    assert_eq!(
        column("https://big.example.org"),
        column("https://disabled.example.org")
    );
    // No colour when not asked for.
    assert!(!text.contains('\x1b'));

    assert!(
        admin::instance_list(Vec::new(), NOW, &settings(), Style::plain())
            .contains("No instances yet")
    );
}

#[test]
fn a_region_names_its_country() {
    assert_eq!(
        admin::area(None, Some("es-ga")).unwrap(),
        ("ES".to_string(), Some("ES-GA".to_string()))
    );
    assert_eq!(
        admin::area(Some("ES"), Some("ES-GA")).unwrap(),
        ("ES".to_string(), Some("ES-GA".to_string()))
    );
    assert_eq!(
        admin::area(Some("es"), None).unwrap(),
        ("ES".to_string(), None)
    );
    // Both, not agreeing; a region that is not; nothing at all.
    assert!(admin::area(Some("PT"), Some("ES-GA")).is_err());
    assert!(admin::area(None, Some("ES-XX")).is_err());
    assert!(admin::area(None, Some("galicia")).is_err());
    assert!(admin::area(None, None).is_err());
    assert!(admin::area(Some(" "), Some("")).is_err());
}
