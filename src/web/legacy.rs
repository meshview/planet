// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Meshview World's addresses, answering in its shapes, so that
//! whatever uses them goes on working. `name` and `source_name`, the old free
//! text area, are the area's English name.

use super::error::ApiError;
use super::v1::{self, AreaNames, TREND_DAYS, Trend};
use super::{AppState, Params, cached, days_back, seen_since_us};
use crate::areas::{Areas, Language};
use crate::db::{self, NodeFilter};
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::LOCATION;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/nodes", get(nodes))
        .route("/api/instances", get(instances).post(v1::add_instance))
        .route("/api/histogram", get(histogram))
        .route("/out/node-link", get(node_link))
}

#[derive(Serialize)]
struct Node {
    source_url: String,
    node_id: i64,
    lat: f64,
    lon: f64,
    source_name: String,
    long_name: Option<String>,
    short_name: Option<String>,
    channel: Option<String>,
    role: Option<String>,
    hw_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_mqtt_gateway: Option<bool>,
    first_seen_us: Option<i64>,
    last_seen_us: Option<i64>,
}

#[derive(Serialize)]
struct Nodes {
    nodes: Vec<Node>,
    total_nodes: usize,
}

/// The nodes with a position, and how many there are with or without one.
async fn nodes(
    State(state): State<AppState>,
    params: Params,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let days = params.days("days_active", 14, 365)?;
    cached(&state, format!("legacy/nodes {days}"), &headers, || async {
        let filter = NodeFilter {
            seen_since_us: seen_since_us(days),
            ..NodeFilter::default()
        };
        let rows = db::nodes(&state.pool, &filter).await?;
        let total_nodes = rows.len();
        let mut names: HashMap<(String, Option<String>), String> = HashMap::new();
        let areas = Areas::get();
        let nodes = rows
            .into_iter()
            .filter_map(|row| {
                let (lat, lon) = (row.lat?, row.lon?);
                let source_name = names
                    .entry((row.country.clone(), row.region.clone()))
                    .or_insert_with(|| {
                        areas.label(&row.country, row.region.as_deref(), Language::English)
                    })
                    .clone();
                Some(Node {
                    source_url: row.instance_url,
                    node_id: row.node_id,
                    lat,
                    lon,
                    source_name,
                    long_name: row.long_name,
                    short_name: row.short_name,
                    channel: row.channel,
                    role: row.role,
                    hw_model: row.hw_model,
                    firmware: row.firmware,
                    is_mqtt_gateway: row.is_mqtt_gateway,
                    first_seen_us: row.first_seen_us,
                    last_seen_us: row.last_seen_us,
                })
            })
            .collect();
        Ok(serde_json::to_vec(&Nodes { nodes, total_nodes }).expect("nodes serialise"))
    })
    .await
}

#[derive(Serialize)]
struct Instance {
    name: String,
    url: String,
    version: Option<String>,
    git_revision_short: Option<String>,
    node_count: i64,
    last_updated_at: i64,
    trend_30d: Trend,
}

#[derive(Serialize)]
struct Instances {
    instances: Vec<Instance>,
}

async fn instances(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    cached(&state, "legacy/instances".into(), &headers, || async {
        let since = days_back(TREND_DAYS - 1);
        let instances = db::instance_summaries(&state.pool, &since)
            .await?
            .into_iter()
            .map(|summary| {
                let trend_30d = Trend::of(&summary);
                let i = summary.instance;
                Instance {
                    name: AreaNames::of(&i.country, i.region.as_deref()).en,
                    last_updated_at: i.last_success_at.unwrap_or(i.added_at),
                    url: i.url,
                    version: i.version,
                    git_revision_short: i.git_revision_short,
                    node_count: summary.node_count,
                    trend_30d,
                }
            })
            .collect();
        Ok(serde_json::to_vec(&Instances { instances }).expect("instances serialise"))
    })
    .await
}

#[derive(Serialize)]
struct Point {
    date: String,
    node_count: i64,
    recorded_at: i64,
}

#[derive(Serialize)]
struct Series {
    name: String,
    url: String,
    points: Vec<Point>,
}

#[derive(Serialize)]
struct Histogram {
    days: i64,
    start_date: String,
    end_date: String,
    instances: Vec<Series>,
}

async fn histogram(
    State(state): State<AppState>,
    params: Params,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let days = params.days("days", 30, 366)?;
    cached(
        &state,
        format!("legacy/histogram {days}"),
        &headers,
        || async {
            let data = v1::histogram_data(&state, days).await?;
            let instances = data
                .series
                .into_iter()
                .map(|(instance, points)| Series {
                    name: AreaNames::of(&instance.country, instance.region.as_deref()).en,
                    url: instance.url,
                    points: points
                        .into_iter()
                        .map(|row| Point {
                            date: row.date,
                            node_count: row.node_count,
                            recorded_at: row.recorded_at,
                        })
                        .collect(),
                })
                .collect();
            Ok(serde_json::to_vec(&Histogram {
                days: data.days,
                start_date: data.start_date,
                end_date: data.end_date,
                instances,
            })
            .expect("the histogram serialises"))
        },
    )
    .await
}

/// Sends the browser to a node's page on the instance that sees it. Only to
/// instances on the list, and only to a node page: anything else would let
/// anyone use this site to send people anywhere.
async fn node_link(State(state): State<AppState>, params: Params) -> Result<Response, ApiError> {
    let source = params
        .text("source_url")
        .ok_or_else(|| ApiError::bad_request("source_url is required"))?;
    let node_id: u32 = params
        .text("node_id")
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| ApiError::bad_request("node_id has to be a node number"))?;
    let url = crate::net::normalise_url(&source, true)
        .map_err(|_| ApiError::not_found("not an instance on the list"))?;
    let instance = db::instance_by_url(&state.pool, &url)
        .await?
        .ok_or_else(|| ApiError::not_found("not an instance on the list"))?;
    let target = format!("{}/node/{node_id}", instance.url);
    Ok((StatusCode::FOUND, [(LOCATION, target)]).into_response())
}
