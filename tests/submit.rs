// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Submitting instances, and importing the old site's list.

mod common;

use common::{Instance, Kind, planet, settings};
use meshview_planet::db;
use meshview_planet::import;
use meshview_planet::net::{Client, FetchError, UrlError};
use meshview_planet::submit::{Rejection, Submission, submit};

const NOW: i64 = 1_794_571_200;

fn submission(url: &str, country: &str, region: Option<&str>) -> Submission {
    Submission {
        url: url.to_string(),
        country: country.to_string(),
        region: region.map(str::to_string),
    }
}

#[tokio::test]
async fn a_meshview_is_accepted_and_published_at_once() {
    let instance = Instance::start(Kind::Rust, 8).await;
    let (_dir, pool, collector) = planet(settings()).await;

    let url = format!("{}/?utm=x", instance.url);
    let accepted = submit(
        &pool,
        &collector,
        &submission(&url, "es", Some("es-ga")),
        NOW,
    )
    .await
    .unwrap();
    assert_eq!(accepted.url, instance.url);
    assert_eq!(accepted.nodes, 8);
    assert_eq!(accepted.version, "5.0.0");

    let stored = db::instance_by_url(&pool, &instance.url)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.country, "ES");
    assert_eq!(stored.region.as_deref(), Some("ES-GA"));
    assert_eq!(stored.last_success_at, Some(NOW));
    assert_eq!(db::count_nodes(&pool, stored.id).await.unwrap(), 8);
}

#[tokio::test]
async fn the_same_instance_is_not_added_twice() {
    let instance = Instance::start(Kind::Python, 2).await;
    let (_dir, pool, collector) = planet(settings()).await;
    submit(
        &pool,
        &collector,
        &submission(&instance.url, "ES", None),
        NOW,
    )
    .await
    .unwrap();

    let again = submit(
        &pool,
        &collector,
        &submission(&format!("{}/", instance.url.to_uppercase()), "PT", None),
        NOW,
    )
    .await;
    assert!(matches!(again, Err(Rejection::Duplicate)), "{again:?}");
}

#[tokio::test]
async fn only_areas_from_the_list_are_accepted() {
    let instance = Instance::start(Kind::Rust, 1).await;
    let (_dir, pool, collector) = planet(settings()).await;
    for (country, region) in [
        ("Nowhere", None),
        ("ES", Some("Somewhere rude")),
        ("ES", Some("PT-11")),
    ] {
        let result = submit(
            &pool,
            &collector,
            &submission(&instance.url, country, region),
            NOW,
        )
        .await;
        assert!(matches!(result, Err(Rejection::Area(_))), "{result:?}");
    }
    assert!(db::instances(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn what_does_not_answer_like_meshview_is_refused() {
    let (_dir, pool, collector) = planet(settings()).await;
    for kind in [Kind::NotMeshview, Kind::Broken, Kind::Huge] {
        let instance = Instance::start(kind, 1).await;
        let result = submit(
            &pool,
            &collector,
            &submission(&instance.url, "ES", None),
            NOW,
        )
        .await;
        assert!(
            matches!(result, Err(Rejection::Probe(_))),
            "{kind:?}: {result:?}"
        );
    }
    assert!(db::instances(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn private_addresses_are_out_of_reach_by_default() {
    let instance = Instance::start(Kind::Rust, 1).await;
    let (_dir, pool, collector) = planet(meshview_planet::config::Collector {
        allow_private_addresses: false,
        ..settings()
    })
    .await;

    let result = submit(
        &pool,
        &collector,
        &submission(&instance.url, "ES", None),
        NOW,
    )
    .await;
    assert!(
        matches!(result, Err(Rejection::Url(UrlError::Private))),
        "{result:?}"
    );

    // Nor does the client itself reach them, by address or by a name that
    // resolves to one.
    let client = Client::new(&meshview_planet::config::Collector {
        allow_private_addresses: false,
        ..settings()
    })
    .unwrap();
    let port = instance.url.rsplit(':').next().unwrap();
    for url in [
        format!("{}/version", instance.url),
        format!("http://localhost:{port}/version"),
    ] {
        let result = client.get(&url).await;
        assert!(
            matches!(result, Err(FetchError::Connection(_))),
            "{url}: {result:?}"
        );
    }
}

#[tokio::test]
async fn the_import_adds_the_instances_it_knows_an_area_for() {
    let old_site = Instance::start(Kind::Rust, 0).await;
    let (_dir, pool, collector) = planet(settings()).await;
    let source = format!("{}/api/instances", old_site.url);

    let imported = import::import(&pool, collector.client(), &source, true, NOW)
        .await
        .unwrap();
    assert_eq!(imported.added, 1);
    assert_eq!(imported.unknown.len(), 2);

    let stored = db::instances(&pool).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].url, "https://meshview.bayme.sh");
    assert_eq!(stored[0].country, "US");
    assert_eq!(stored[0].region.as_deref(), Some("US-CA"));
    assert_eq!(stored[0].added_at, NOW);

    // With its history, but for the day it read no nodes and the one of no
    // form.
    assert_eq!((imported.days, imported.history_error), (2, None));
    assert_eq!(
        db::daily_counts(&pool, stored[0].id).await.unwrap(),
        [
            ("2026-11-10".to_string(), 120),
            ("2026-11-13".to_string(), 125)
        ]
    );

    // Twice is harmless.
    let again = import::import(&pool, collector.client(), &source, true, NOW)
        .await
        .unwrap();
    assert_eq!((again.added, again.already_there, again.days), (0, 1, 0));
}

#[tokio::test]
async fn the_import_does_without_the_history() {
    // A list read from somewhere else: no history beside it.
    let old_site = Instance::start(Kind::Rust, 0).await;
    let (_dir, pool, collector) = planet(settings()).await;
    let source = format!("{}/api/instances?copy", old_site.url);
    let imported = import::import(&pool, collector.client(), &source, true, NOW)
        .await
        .unwrap();
    assert_eq!(imported.added, 1);
    assert_eq!(imported.days, 0);
    assert!(imported.history_error.unwrap().contains("not known"));
}
