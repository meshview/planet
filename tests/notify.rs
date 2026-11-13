// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Notices, sent to a pretend receiver that plays every channel.

mod common;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::routing::any;
use common::{Instance, Kind, planet, settings};
use meshview_planet::collector::unix_now;
use meshview_planet::config::{Config, Event, Notifications};
use meshview_planet::db;
use meshview_planet::notify::{Channel, Notice, Notifier};
use meshview_planet::web::{AppState, router};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tower::ServiceExt;

/// What the receiver got: method, path, headers and body.
type Received = Arc<Mutex<Vec<(Method, String, HeaderMap, Value)>>>;

/// A receiver for every channel, answering 200 to everything but `/broken`.
async fn receiver() -> (String, Received) {
    let received: Received = Arc::default();
    let app = Router::new()
        .fallback(any(
            |State(received): State<Received>, request: Request| async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                let path = parts.uri.path().to_string();
                let status = if path == "/broken" {
                    StatusCode::BAD_GATEWAY
                } else {
                    StatusCode::OK
                };
                received
                    .lock()
                    .unwrap()
                    .push((parts.method, path, parts.headers, body));
                status
            },
        ))
        .with_state(received.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), received)
}

fn every_channel(base: &str, events: Vec<Event>) -> Notifications {
    Notifications {
        events,
        webhook_url: format!("{base}/hook"),
        ntfy_url: format!("{base}/my-topic"),
        ntfy_token: "tk_secret".into(),
        discord_url: format!("{base}/discord"),
        slack_url: format!("{base}/slack"),
        matrix_homeserver: format!("{base}/"),
        matrix_room: "!room:example.org".into(),
        matrix_token: "syt_secret".into(),
    }
}

fn added() -> Notice {
    Notice::added(
        "https://meshview.example.org",
        "ES",
        Some("ES-GA"),
        "5.0.0",
        812,
        "203.0.113.7".parse().unwrap(),
        1_794_571_200,
    )
}

#[tokio::test]
async fn every_channel_gets_the_notice_in_its_own_shape() {
    let (base, received) = receiver().await;
    let notifier = Notifier::new(&every_channel(&base, vec![Event::InstanceAdded])).unwrap();
    let results = notifier.deliver(&added()).await;
    assert_eq!(results.len(), 5);
    for (channel, result) in &results {
        assert!(result.is_ok(), "{}: {result:?}", channel.name());
    }

    let received = received.lock().unwrap();
    let find = |path: &str| {
        received
            .iter()
            .find(|(_, p, ..)| p.starts_with(path))
            .unwrap_or_else(|| panic!("nothing at {path}"))
            .clone()
    };

    let (method, _, _, hook) = find("/hook");
    assert_eq!(method, Method::POST);
    assert_eq!(hook["event"], "instance_added");
    assert_eq!(hook["instance"]["area"]["en"], "Galicia, Spain");
    assert_eq!(hook["from_ip"], "203.0.113.7");

    // ntfy's JSON goes to the server, with the topic inside.
    let (_, _, ntfy_headers, ntfy) = received
        .iter()
        .find(|(_, path, ..)| path == "/")
        .cloned()
        .expect("nothing at the ntfy server");
    assert_eq!(ntfy["topic"], "my-topic");
    assert_eq!(ntfy["title"], "New instance: Galicia, Spain");
    assert_eq!(ntfy["click"], "https://meshview.example.org");
    assert_eq!(ntfy["tags"], json!(["new"]));
    assert_eq!(ntfy_headers[header::AUTHORIZATION], "Bearer tk_secret");

    let (_, _, _, discord) = find("/discord");
    assert!(
        discord["content"]
            .as_str()
            .unwrap()
            .starts_with("**New instance: Galicia, Spain**\nhttps://meshview.example.org")
    );
    assert_eq!(discord["allowed_mentions"], json!({"parse": []}));

    let (_, _, _, slack) = find("/slack");
    assert!(
        slack["text"]
            .as_str()
            .unwrap()
            .starts_with("*New instance: Galicia, Spain*")
    );

    let (method, path, headers, matrix) = find("/_matrix/");
    assert_eq!(method, Method::PUT);
    assert!(
        path.starts_with("/_matrix/client/v3/rooms/%21room%3Aexample.org/send/m.room.message/"),
        "{path}"
    );
    assert_eq!(headers[header::AUTHORIZATION], "Bearer syt_secret");
    assert_eq!(matrix["msgtype"], "m.text");
}

#[tokio::test]
async fn a_channel_that_fails_says_why_and_the_others_still_get_it() {
    let (base, received) = receiver().await;
    let notifier = Notifier::new(&Notifications {
        webhook_url: format!("{base}/broken"),
        slack_url: format!("{base}/slack"),
        ..Notifications::default()
    })
    .unwrap();
    let results = notifier.deliver(&added()).await;
    let webhook = &results
        .iter()
        .find(|(c, _)| *c == Channel::Webhook)
        .unwrap()
        .1;
    assert!(format!("{:?}", webhook.as_ref().unwrap_err()).contains("502"));
    assert!(
        results
            .iter()
            .find(|(c, _)| *c == Channel::Slack)
            .unwrap()
            .1
            .is_ok()
    );
    assert_eq!(received.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn only_the_events_asked_for_are_sent() {
    let (base, received) = receiver().await;
    let notifier = Notifier::new(&Notifications {
        webhook_url: format!("{base}/hook"),
        ..Notifications::default()
    })
    .unwrap();
    notifier.send(Notice::offline(
        "https://x.example.org",
        "US",
        None,
        None,
        3,
        0,
    ));
    notifier.send(added());
    notifier.flush().await;
    let received = received.lock().unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].3["event"], "instance_added");
}

#[tokio::test]
async fn a_submission_from_the_site_is_told_with_its_address() {
    let (base, received) = receiver().await;
    let mut config = Config::default();
    config.collector = settings();
    config.server.behind_proxy = true;
    config.notifications.webhook_url = format!("{base}/hook");
    let (_dir, pool, collector) = planet(config.collector.clone()).await;
    let notifier = Notifier::new(&config.notifications).unwrap();
    let collector = collector.with_notifier(notifier.clone());
    let app = router(AppState::new(pool, collector, config));

    let instance = Instance::start(Kind::Rust, 10).await;
    let response = app
        .oneshot(
            axum::http::Request::post("/api/v1/instances")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-forwarded-for", "198.51.100.9")
                .body(Body::from(
                    json!({"url": instance.url, "country": "PT"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    notifier.flush().await;

    let received = received.lock().unwrap();
    assert_eq!(received.len(), 1);
    let notice = &received[0].3;
    assert_eq!(notice["event"], "instance_added");
    assert_eq!(notice["instance"]["url"], instance.url.as_str());
    assert_eq!(notice["instance"]["area"]["en"], "Portugal");
    assert_eq!(notice["instance"]["node_count"], 10);
    assert_eq!(notice["from_ip"], "198.51.100.9");
}

#[tokio::test]
async fn the_collector_tells_of_instances_going_and_coming_back() {
    let (base, received) = receiver().await;
    let notifications = Notifications {
        events: vec![
            Event::InstanceOffline,
            Event::InstanceOnline,
            Event::InstanceDeleted,
        ],
        webhook_url: format!("{base}/hook"),
        ..Notifications::default()
    };
    let notifier = Notifier::new(&notifications).unwrap();
    let (_dir, pool, collector) = planet(settings()).await;
    let collector = collector.with_notifier(notifier.clone());
    let instance = Instance::start(Kind::Broken, 4).await;
    let now = unix_now();
    db::add_instance(&pool, &instance.url, "ES", Some("ES-GA"), now)
        .await
        .unwrap();

    // Silent for long enough: offline.
    collector.round(now + 3 * 86_400 + 1).await.unwrap();
    // Back.
    instance.set(Kind::Rust);
    collector.round(now + 4 * 86_400).await.unwrap();
    // Gone again, and for good.
    instance.set(Kind::Broken);
    collector.round(now + 8 * 86_400).await.unwrap();
    collector.round(now + 39 * 86_400).await.unwrap();
    notifier.flush().await;

    let received = received.lock().unwrap();
    let events: Vec<&str> = received
        .iter()
        .map(|(.., body)| body["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        events,
        [
            "instance_offline",
            "instance_online",
            "instance_offline",
            "instance_deleted"
        ]
    );
    let first = &received[0].3;
    assert_eq!(first["days"], 3);
    assert_eq!(
        first["instance"]["last_error"],
        "/version: answered HTTP 500"
    );
    assert_eq!(received[1].3["instance"]["node_count"], 4);
    assert_eq!(received[1].3["instance"]["version"], "5.0.0");
}
