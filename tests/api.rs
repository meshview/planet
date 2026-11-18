// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The web API, both versions, called the way axum calls it.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{Instance, Kind, planet, settings};
use http_body_util::BodyExt;
use meshview_planet::collector::{Collector, unix_now};
use meshview_planet::config::Config;
use meshview_planet::db;
use meshview_planet::web::cache::ResponseCache;
use meshview_planet::web::{AppState, router};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::time::Duration;
use tempfile::TempDir;
use tower::ServiceExt;

struct App {
    _dir: TempDir,
    pool: SqlitePool,
    collector: Collector,
    router: Router,
}

async fn app_with(config: Config) -> App {
    let (dir, pool, collector) = planet(config.collector.clone()).await;
    let mut state = AppState::new(pool.clone(), collector.clone(), config);
    // Every change seen at once, rather than a few seconds later.
    state.cache = std::sync::Arc::new(ResponseCache::with_min_age(Duration::ZERO));
    let router = router(state);
    App {
        _dir: dir,
        pool,
        collector,
        router,
    }
}

fn test_config() -> Config {
    let mut config = Config::default();
    config.collector = settings();
    config
}

async fn app() -> App {
    app_with(test_config()).await
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&self.body)))
    }
}

impl App {
    async fn send(&self, request: Request<Body>) -> Answer {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Answer {
            status,
            headers,
            body: body.to_vec(),
        }
    }

    async fn get(&self, uri: &str) -> Answer {
        self.send(Request::get(uri).body(Body::empty()).unwrap())
            .await
    }

    async fn post(&self, uri: &str, body: Value) -> Answer {
        self.send(
            Request::post(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    /// Adds an instance and collects it now.
    async fn add(&self, url: &str, country: &str, region: Option<&str>) -> i64 {
        let id = db::add_instance(&self.pool, url, country, region, unix_now())
            .await
            .unwrap()
            .unwrap();
        self.collector.collect_all(unix_now()).await.unwrap();
        id
    }
}

// With ten nodes, the pretend instances have: two last heard twenty days ago
// (4 and 9), and three without a position (2, 5 and 8). In fourteen days that
// is eight nodes, five of them mapped.

#[tokio::test]
async fn nodes_in_both_shapes() {
    let app = app().await;
    let galiza = Instance::start(Kind::Rust, 10).await;
    app.add(&galiza.url, "ES", Some("ES-GA")).await;

    let v1 = app.get("/api/v1/nodes").await;
    assert_eq!(v1.status, StatusCode::OK);
    let v1 = v1.json();
    assert_eq!(
        (v1["total"].as_u64(), v1["mapped"].as_u64()),
        (Some(8), Some(5))
    );
    assert_eq!(v1["nodes"].as_array().unwrap().len(), 8);
    let first = &v1["nodes"][0];
    assert_eq!(first["node_id"], 1000);
    assert_eq!(first["id"], "!000003e8");
    assert_eq!(first["instance_url"], galiza.url.as_str());
    assert_eq!(first["lat"], 42.0);
    assert_eq!(first["is_mqtt_gateway"], true);

    let legacy = app.get("/api/nodes?days_active=14").await.json();
    assert_eq!(legacy["total_nodes"], 8);
    let nodes = legacy["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 5, "only those with a position");
    assert_eq!(nodes[0]["source_url"], galiza.url.as_str());
    assert_eq!(nodes[0]["source_name"], "Galicia, Spain");
    // As the old site: firmware only when there is one.
    assert!(nodes[0].get("firmware").is_none());
    assert_eq!(nodes[1]["lon"], -8.0000001);

    let month = app.get("/api/nodes?days_active=30").await.json();
    assert_eq!(month["total_nodes"], 10);
    assert_eq!(month["nodes"].as_array().unwrap().len(), 7);
}

#[tokio::test]
async fn nodes_can_be_filtered() {
    let app = app().await;
    let galiza = Instance::start(Kind::Rust, 10).await;
    let portugal = Instance::start(Kind::Python, 10).await;
    app.add(&galiza.url, "ES", Some("ES-GA")).await;
    app.add(&portugal.url, "PT", None).await;

    let count = |answer: Answer| answer.json()["nodes"].as_array().unwrap().len();
    assert_eq!(count(app.get("/api/v1/nodes").await), 16);
    assert_eq!(count(app.get("/api/v1/nodes?country=pt").await), 8);
    assert_eq!(count(app.get("/api/v1/nodes?region=ES-GA").await), 8);
    assert_eq!(count(app.get("/api/v1/nodes?has_position=true").await), 10);
    assert_eq!(count(app.get("/api/v1/nodes?has_position=false").await), 6);
    assert_eq!(count(app.get("/api/v1/nodes?channel=LongFast").await), 16);
    assert_eq!(count(app.get("/api/v1/nodes?channel=ShortFast").await), 0);
    let instance = format!("/api/v1/nodes?instance={}/", galiza.url);
    assert_eq!(count(app.get(&instance).await), 8);
    assert_eq!(count(app.get("/api/v1/nodes?days_active=1000").await), 20);

    let bad = app.get("/api/v1/nodes?days_active=many").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.json()["code"], "bad_request");
    let bad = app.get("/api/v1/nodes?has_position=maybe").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn offline_instances_are_last_and_off_the_map() {
    let app = app().await;
    let up = Instance::start(Kind::Rust, 10).await;
    let down = Instance::start(Kind::Python, 20).await;
    app.add(&up.url, "ES", Some("ES-GA")).await;
    app.add(&down.url, "US", Some("US-CA")).await;

    // The larger one comes first while both answer.
    let listed = app.get("/api/v1/instances").await.json();
    assert_eq!(listed["instances"][0]["url"], down.url.as_str());

    down.set(Kind::Broken);
    let later = unix_now() + 4 * 86_400;
    app.collector.round(later).await.unwrap();

    let listed = app.get("/api/v1/instances").await.json();
    let instances = listed["instances"].as_array().unwrap();
    assert_eq!(instances[0]["url"], up.url.as_str());
    assert_eq!(instances[0]["status"], "online");
    assert_eq!(instances[1]["url"], down.url.as_str());
    assert_eq!(instances[1]["status"], "offline");
    assert_eq!(instances[1]["offline_since"], later);
    assert_eq!(instances[1]["last_error"], "/version: answered HTTP 500");
    // It keeps its nodes, but they are not on the map.
    assert_eq!(instances[1]["node_count"], 20);
    let nodes = app.get("/api/v1/nodes").await.json();
    assert_eq!(nodes["total"], 8);
    let legacy = app.get("/api/nodes").await.json();
    assert_eq!(legacy["total_nodes"], 8);
}

#[tokio::test]
async fn instances_in_both_shapes() {
    let app = app().await;
    let galiza = Instance::start(Kind::Rust, 10).await;
    app.add(&galiza.url, "ES", Some("ES-GA")).await;
    galiza.set_nodes(12);
    app.collector.collect_all(unix_now()).await.unwrap();

    let v1 = app.get("/api/v1/instances").await.json();
    let instance = &v1["instances"][0];
    assert_eq!(instance["country"], "ES");
    assert_eq!(instance["region"], "ES-GA");
    assert_eq!(
        instance["area"],
        json!({"en": "Galicia, Spain", "es": "Galicia, España"})
    );
    assert_eq!(instance["version"], "5.0.0");
    assert_eq!(instance["git_revision_short"], "21714da");
    assert_eq!(instance["flavor"], "rust");
    assert_eq!(instance["node_count"], 12);
    assert_eq!(instance["mapped_count"], 8);
    // Both collections were today: one daily count, the latest.
    assert_eq!(
        instance["trend_30d"],
        json!({"start_count": 12, "end_count": 12})
    );
    assert_eq!(instance["last_error"], Value::Null);

    let legacy = app.get("/api/instances").await.json();
    let instance = legacy["instances"][0].as_object().unwrap();
    let mut keys: Vec<&str> = instance.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "git_revision_short",
            "last_updated_at",
            "name",
            "node_count",
            "trend_30d",
            "url",
            "version"
        ]
    );
    assert_eq!(instance["name"], "Galicia, Spain");
}

#[tokio::test]
async fn the_history_in_both_shapes() {
    let app = app().await;
    let galiza = Instance::start(Kind::Rust, 10).await;
    let id = app.add(&galiza.url, "ES", Some("ES-GA")).await;
    // Yesterday, as if it had been collected then.
    sqlx::query(
        "INSERT INTO instance_daily (instance_id, date, node_count, recorded_at)
         VALUES (?, ?, 7, ?)",
    )
    .bind(id)
    .bind(db::utc_date_days_before(unix_now(), 1))
    .bind(unix_now() - 86_400)
    .execute(&app.pool)
    .await
    .unwrap();

    let v1 = app.get("/api/v1/histogram?days=7").await.json();
    assert_eq!(v1["days"], 7);
    assert_eq!(v1["end_date"], db::utc_date(unix_now()));
    assert_eq!(v1["start_date"], db::utc_date_days_before(unix_now(), 6));
    let points = v1["instances"][0]["points"].as_array().unwrap();
    let counts: Vec<i64> = points
        .iter()
        .map(|p| p["node_count"].as_i64().unwrap())
        .collect();
    assert_eq!(counts, [7, 10]);
    assert_eq!(v1["instances"][0]["region"], "ES-GA");

    // A single day leaves yesterday out.
    let today = app.get("/api/histogram?days=1").await.json();
    assert_eq!(today["instances"][0]["name"], "Galicia, Spain");
    assert_eq!(today["instances"][0]["points"].as_array().unwrap().len(), 1);

    // And the trend sees both.
    let instances = app.get("/api/v1/instances").await.json();
    assert_eq!(
        instances["instances"][0]["trend_30d"],
        json!({"start_count": 7, "end_count": 10})
    );
}

#[tokio::test]
async fn answers_carry_an_etag_and_come_gzipped() {
    let app = app().await;
    let galiza = Instance::start(Kind::Rust, 10).await;
    app.add(&galiza.url, "ES", None).await;

    let first = app.get("/api/v1/nodes").await;
    let etag = first.headers[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(first.headers[header::CACHE_CONTROL], "no-cache");

    let again = app
        .send(
            Request::get("/api/v1/nodes")
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    assert!(again.body.is_empty());

    let gzipped = app
        .send(
            Request::get("/api/v1/nodes")
                .header(header::ACCEPT_ENCODING, "gzip, br")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(gzipped.headers[header::CONTENT_ENCODING], "gzip");
    let mut plain = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::GzDecoder::new(&gzipped.body[..]),
        &mut plain,
    )
    .unwrap();
    assert_eq!(plain, first.body);

    // A new collection is new data, and a new tag.
    galiza.set_nodes(3);
    app.collector.collect_all(unix_now() + 1).await.unwrap();
    let after = app
        .send(
            Request::get("/api/v1/nodes")
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(after.status, StatusCode::OK);
    assert_ne!(after.headers[header::ETAG], etag.as_str());
    assert_eq!(after.json()["total"], 3);
}

#[tokio::test]
async fn reads_are_open_to_any_page() {
    let app = app().await;
    let answer = app
        .send(
            Request::get("/api/v1/instances")
                .header(header::ORIGIN, "https://example.org")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(answer.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
}

#[tokio::test]
async fn an_instance_is_added_from_the_api() {
    let app = app().await;
    let galiza = Instance::start(Kind::Rust, 10).await;

    let added = app
        .post(
            "/api/v1/instances",
            json!({"url": format!("{}/", galiza.url), "country": "es", "region": "es-ga"}),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.json());
    let added = added.json();
    assert_eq!(added["url"], galiza.url.as_str());
    assert_eq!(added["region"], "ES-GA");
    assert_eq!(added["area"]["es"], "Galicia, España");
    assert_eq!(added["node_count"], 10);

    // On the map at once.
    let nodes = app.get("/api/v1/nodes").await.json();
    assert_eq!(nodes["total"], 8);

    let again = app
        .post(
            "/api/instances",
            json!({"url": galiza.url, "country": "ES"}),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.json()["code"], "duplicate");
}

#[tokio::test]
async fn submissions_are_refused_with_a_reason() {
    // More attempts than the default limit allows from one address.
    let mut config = test_config();
    config.submissions.per_ip_per_hour = 100;
    let app = app_with(config).await;
    let meshview = Instance::start(Kind::Rust, 1).await;
    let website = Instance::start(Kind::NotMeshview, 0).await;

    for (body, status, code) in [
        (
            json!({"name": "Somewhere", "url": meshview.url}),
            StatusCode::BAD_REQUEST,
            "area_required",
        ),
        (
            json!({"url": meshview.url, "country": "XX"}),
            StatusCode::BAD_REQUEST,
            "unknown_country",
        ),
        (
            json!({"url": meshview.url, "country": "ES", "region": "Nowhere"}),
            StatusCode::BAD_REQUEST,
            "unknown_region",
        ),
        (
            json!({"url": "ftp://example.org", "country": "ES"}),
            StatusCode::BAD_REQUEST,
            "invalid_url",
        ),
        (
            json!({"url": website.url, "country": "ES"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_meshview",
        ),
        (json!({"nope": 1}), StatusCode::BAD_REQUEST, "bad_request"),
    ] {
        let answer = app.post("/api/v1/instances", body.clone()).await;
        assert_eq!(answer.status, status, "{body}");
        assert_eq!(answer.json()["code"], code, "{body}");
    }
    assert!(db::instances(&app.pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn submissions_are_limited_per_address() {
    let mut config = test_config();
    config.submissions.per_ip_per_hour = 2;
    config.server.behind_proxy = true;
    let app = app_with(config).await;

    let from = |ip: &str| {
        Request::post("/api/v1/instances")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", format!("1.2.3.4, {ip}"))
            .body(Body::from(
                json!({"url": "ftp://x", "country": "ES"}).to_string(),
            ))
            .unwrap()
    };
    assert_eq!(
        app.send(from("203.0.113.1")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.send(from("203.0.113.1")).await.status,
        StatusCode::BAD_REQUEST
    );
    let limited = app.send(from("203.0.113.1")).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.json()["code"], "rate_limited");
    // The proxy saw another address: another count.
    assert_eq!(
        app.send(from("203.0.113.2")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn submissions_can_be_turned_off() {
    let mut config = test_config();
    config.submissions.enabled = false;
    let app = app_with(config).await;
    let answer = app
        .post("/api/v1/instances", json!({"url": "x", "country": "ES"}))
        .await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN);
    assert_eq!(answer.json()["code"], "disabled");
}

#[tokio::test]
async fn node_links_go_only_to_listed_instances() {
    let app = app().await;
    let galiza = Instance::start(Kind::Rust, 1).await;
    app.add(&galiza.url, "ES", None).await;

    let link = app
        .get(&format!(
            "/out/node-link?source_url={}/&node_id=1000",
            galiza.url
        ))
        .await;
    assert_eq!(link.status, StatusCode::FOUND);
    assert_eq!(
        link.headers[header::LOCATION],
        format!("{}/node/1000", galiza.url).as_str()
    );

    let elsewhere = app
        .get("/out/node-link?source_url=https://evil.example&node_id=1")
        .await;
    assert_eq!(elsewhere.status, StatusCode::NOT_FOUND);
    let not_a_node = app
        .get(&format!(
            "/out/node-link?source_url={}&node_id=../../admin",
            galiza.url
        ))
        .await;
    assert_eq!(not_a_node.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_areas_to_choose_from() {
    let app = app().await;
    let english = app.get("/api/v1/areas").await.json();
    let spain = english["countries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "ES")
        .unwrap();
    assert_eq!(spain["name"], "Spain");
    assert!(
        spain["regions"]
            .as_array()
            .unwrap()
            .contains(&json!({"code": "ES-GA", "name": "Galicia"}))
    );

    let spanish = app.get("/api/v1/areas?lang=es").await.json();
    assert!(
        spanish["countries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "España")
    );

    assert_eq!(
        app.get("/api/v1/areas?lang=ru").await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn the_reference_documents_the_api() {
    let app = app().await;
    let spec = app.get("/api/openapi.json").await.json();
    let paths = spec["paths"].as_object().unwrap();
    for path in [
        "/livez",
        "/version",
        "/api/v1/nodes",
        "/api/v1/instances",
        "/api/v1/histogram",
        "/api/v1/areas",
    ] {
        assert!(paths.contains_key(path), "{path}");
    }
    assert!(paths["/api/v1/instances"].get("post").is_some());
    let docs = app.get("/api/docs").await;
    assert_eq!(docs.status, StatusCode::OK);

    let missing = app.get("/api/v1/nothing").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.json()["code"], "not_found");
}

#[tokio::test]
async fn a_round_does_not_rebuild_on_every_instance_but_a_submission_shows_at_once() {
    let config = test_config();
    let (_dir, pool, collector) = planet(config.collector.clone()).await;
    // The cache as it runs, with its minimum age.
    let app = App {
        router: router(AppState::new(pool.clone(), collector.clone(), config)),
        _dir,
        pool,
        collector,
    };
    let first = Instance::start(Kind::Rust, 10).await;
    app.add(&first.url, "ES", None).await;

    let before = app.get("/api/v1/nodes").await;
    first.set_nodes(3);
    app.collector.collect_all(unix_now() + 1).await.unwrap();
    // Moments later, the same answer, with its own tag.
    let soon = app.get("/api/v1/nodes").await;
    assert_eq!(soon.body, before.body);
    assert_eq!(soon.headers[header::ETAG], before.headers[header::ETAG]);

    // But an instance added here is there at once.
    let second = Instance::start(Kind::Python, 10).await;
    let added = app
        .post(
            "/api/v1/instances",
            json!({"url": second.url, "country": "PT"}),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED);
    let after = app.get("/api/v1/nodes").await.json();
    assert_eq!(after["total"], 3 + 8);
}

#[tokio::test]
async fn a_disabled_instance_is_off_the_site_and_refused() {
    let app = app().await;
    let shown = Instance::start(Kind::Rust, 10).await;
    let hidden = Instance::start(Kind::Python, 20).await;
    app.add(&shown.url, "ES", Some("ES-GA")).await;
    app.add(&hidden.url, "PT", None).await;
    assert_eq!(
        app.get("/api/v1/instances").await.json()["instances"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    db::set_disabled(&app.pool, &hidden.url, Some(unix_now()))
        .await
        .unwrap();

    for uri in ["/api/v1/instances", "/api/instances"] {
        let listed = app.get(uri).await.json();
        let instances = listed["instances"].as_array().unwrap();
        assert_eq!(instances.len(), 1, "{uri}");
        assert_eq!(instances[0]["url"], shown.url.as_str(), "{uri}");
    }
    assert_eq!(app.get("/api/v1/nodes").await.json()["total"], 8);
    assert_eq!(app.get("/api/nodes").await.json()["total_nodes"], 8);
    let history = app.get("/api/v1/histogram?days=7").await.json();
    assert_eq!(history["instances"].as_array().unwrap().len(), 1);
    let link = app
        .get(&format!(
            "/out/node-link?source_url={}&node_id=1",
            hidden.url
        ))
        .await;
    assert_eq!(link.status, StatusCode::NOT_FOUND);

    let again = app
        .post(
            "/api/v1/instances",
            json!({"url": hidden.url, "country": "PT"}),
        )
        .await;
    assert_eq!(again.status, StatusCode::FORBIDDEN);
    assert_eq!(again.json()["code"], "blocked");

    // A round leaves it alone, and so does the cleanup, however long.
    let before = db::instance_by_url(&app.pool, &hidden.url)
        .await
        .unwrap()
        .unwrap();
    app.collector.round(unix_now() + 60 * 86_400).await.unwrap();
    let after = db::instance_by_url(&app.pool, &hidden.url)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.last_attempt_at, before.last_attempt_at);
    assert_eq!(after.offline_since, None);
    assert_eq!(db::count_nodes(&app.pool, after.id).await.unwrap(), 20);
}
