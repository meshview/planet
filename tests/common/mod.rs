// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Pretend Meshview instances, on local ports, for the tests.

#![allow(dead_code)]

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use meshview_planet::collector::Collector;
use meshview_planet::config::Collector as Settings;
use serde_json::json;
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio::net::TcpListener;

/// How a pretend instance behaves; can be changed while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Rust Meshview: /api/v1/nodes, and /api/nodes redirecting there.
    Rust,
    /// Python Meshview: /api/nodes only.
    Python,
    /// A fork that answers only `version` in /version, as "plus" does.
    Plus,
    /// Answers 500 to everything.
    Broken,
    /// A web site that is not Meshview, answering a page for every path.
    NotMeshview,
    /// A Meshview answering a nodes list far larger than any limit.
    Huge,
}

#[derive(Clone)]
struct Behaviour {
    kind: Arc<Mutex<Kind>>,
    nodes: Arc<Mutex<usize>>,
}

pub struct Instance {
    pub url: String,
    behaviour: Behaviour,
}

impl Instance {
    pub async fn start(kind: Kind, nodes: usize) -> Self {
        let behaviour = Behaviour {
            kind: Arc::new(Mutex::new(kind)),
            nodes: Arc::new(Mutex::new(nodes)),
        };
        let app = Router::new()
            .route("/version", get(version))
            .route("/api/v1/nodes", get(v1_nodes))
            .route("/api/nodes", get(legacy_nodes))
            .route("/api/instances", get(old_instances))
            .route("/api/histogram", get(old_histogram))
            .fallback(get(fallback))
            .with_state(behaviour.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url: format!("http://{addr}"),
            behaviour,
        }
    }

    pub fn set(&self, kind: Kind) {
        *self.behaviour.kind.lock().unwrap() = kind;
    }

    pub fn set_nodes(&self, nodes: usize) {
        *self.behaviour.nodes.lock().unwrap() = nodes;
    }
}

fn page() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html")],
        "<html><body>Hello</body></html>",
    )
        .into_response()
}

async fn version(State(b): State<Behaviour>) -> Response {
    match *b.kind.lock().unwrap() {
        Kind::Broken => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Kind::NotMeshview => page(),
        Kind::Plus => axum::Json(json!({"version": "plus 1.0.0"})).into_response(),
        Kind::Rust => axum::Json(json!({
            "version": "5.0.0", "release_date": "2026-11-13",
            "git_revision": "21714da68cd669122f18abbf798341dee3f32d1b",
            "git_revision_short": "21714da", "flavor": "rust"
        }))
        .into_response(),
        Kind::Python | Kind::Huge => axum::Json(json!({
            "version": "4.0.0", "release_date": "2026-11-01",
            "git_revision": "unknown", "git_revision_short": "unknown"
        }))
        .into_response(),
    }
}

/// Nodes heard an hour ago, by the real clock, except every fifth, heard
/// twenty days ago: the web counts days back from now.
fn nodes_body(count: usize) -> serde_json::Value {
    let now_us = meshview_planet::collector::unix_now() * 1_000_000;
    let hour_us = 3_600 * 1_000_000;
    let nodes: Vec<_> = (0..count)
        .map(|i| {
            // Every third node has no position.
            let (lat, lon) = if i % 3 == 2 {
                (serde_json::Value::Null, serde_json::Value::Null)
            } else {
                (json!(420000000 + i as i64), json!(-80000000 - i as i64))
            };
            json!({
                "id": format!("!{:08x}", 1000 + i),
                "node_id": 1000 + i,
                "long_name": format!("Node {i}"),
                "short_name": format!("N{i}"),
                "hw_model": "HELTEC_V3",
                "firmware": null,
                "role": "CLIENT",
                "last_lat": lat,
                "last_long": lon,
                "channel": "LongFast",
                "is_mqtt_gateway": i == 0,
                "first_seen_us": 1_700_000_000_000_000_i64,
                "last_seen_us": if i % 5 == 4 {
                    now_us - 20 * 24 * hour_us
                } else {
                    now_us - hour_us
                },
            })
        })
        .collect();
    json!({ "nodes": nodes })
}

async fn v1_nodes(State(b): State<Behaviour>) -> Response {
    let kind = *b.kind.lock().unwrap();
    let count = *b.nodes.lock().unwrap();
    match kind {
        Kind::Rust => axum::Json(nodes_body(count)).into_response(),
        Kind::Broken => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Kind::NotMeshview => page(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn legacy_nodes(State(b): State<Behaviour>, uri: axum::http::Uri) -> Response {
    let kind = *b.kind.lock().unwrap();
    let count = *b.nodes.lock().unwrap();
    match kind {
        Kind::Rust => Redirect::permanent(&format!(
            "/api/v1/nodes?{}",
            uri.query().unwrap_or_default()
        ))
        .into_response(),
        Kind::Python | Kind::Plus => axum::Json(nodes_body(count)).into_response(),
        Kind::Huge => {
            // Over a megabyte of padding in one string.
            let padding = "x".repeat(2 * 1024 * 1024);
            axum::Json(json!({"nodes": [], "padding": padding})).into_response()
        }
        Kind::Broken => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Kind::NotMeshview => page(),
    }
}

/// The original site's list: one instance it knows an area for, one it
/// does not, and one that is not a URL at all.
async fn old_instances() -> Response {
    axum::Json(json!({"instances": [
        {"name": "San Francisco Bay Area", "url": "https://meshview.bayme.sh/"},
        {"name": "Somewhere new", "url": "https://meshview.example.org"},
        {"name": "Broken", "url": "ftp://nope"},
    ]}))
    .into_response()
}

/// The old site's history: two good days, a day it read no nodes, which was
/// a failure, a date of no form, and an instance not imported.
async fn old_histogram() -> Response {
    axum::Json(
        json!({"days": 3650, "start_date": "2016-11-16", "end_date": "2026-11-13", "instances": [
            {"name": "San Francisco Bay Area", "url": "https://meshview.bayme.sh", "points": [
                {"date": "2026-11-10", "node_count": 120, "recorded_at": 1_794_300_000},
                {"date": "2026-11-11", "node_count": 0, "recorded_at": 1_794_390_000},
                {"date": "11/12/2026", "node_count": 130, "recorded_at": 1_794_480_000},
                {"date": "2026-11-13", "node_count": 125, "recorded_at": 1_794_570_000},
            ]},
            {"name": "Somewhere new", "url": "https://meshview.example.org", "points": [
                {"date": "2026-11-13", "node_count": 9, "recorded_at": 1_794_570_000},
            ]},
        ]}),
    )
    .into_response()
}

async fn fallback(State(b): State<Behaviour>) -> Response {
    match *b.kind.lock().unwrap() {
        Kind::NotMeshview => page(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Collector settings for tests: local addresses allowed, a short timeout and
/// a one megabyte answer limit.
pub fn settings() -> Settings {
    Settings {
        allow_private_addresses: true,
        timeout_seconds: 5,
        max_response_mb: 1,
        ..Settings::default()
    }
}

/// A fresh database in a directory that goes away with it, and a collector on
/// it.
pub async fn planet(settings: Settings) -> (TempDir, SqlitePool, Collector) {
    let dir = TempDir::new().unwrap();
    let pool = meshview_planet::db::open(&dir.path().join("planet.db"))
        .await
        .unwrap();
    let collector = Collector::new(pool.clone(), settings).unwrap();
    (dir, pool, collector)
}
