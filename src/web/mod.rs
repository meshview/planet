// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The web server: the API, documented under `/api/v1` and in the old site's
//! shapes at its old addresses, and the reference at `/api/docs`.

pub mod cache;
pub mod error;
mod legacy;
pub mod v1;

use crate::collector::{Collector, unix_now};
use crate::config::Config;
use crate::submit::RateLimiter;
use axum::extract::{ConnectInfo, FromRequestParts, Query};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use cache::ResponseCache;
use error::ApiError;
use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_scalar::{Scalar, Servable};

const DOCS_PATH: &str = "/api/docs";
const OPENAPI_PATH: &str = "/api/openapi.json";

/// Scalar, as Meshview serves it.
const SCALAR_HTML: &str = r#"<!doctype html>
<html>
  <head>
    <title>Meshview Planet API</title>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
  </head>
  <body>
    <script id="api-reference" type="application/json"
      data-configuration='{"operationTitleSource":"path"}'>$spec</script>
    <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.76.4"></script>
  </body>
</html>
"#;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Meshview Planet",
        description = "The nodes every listed Meshview instance sees, and the instances \
            themselves.\n\nEvery `GET` can be called from any web page (CORS is open), and its \
            answer carries an ETag that changes only when the data does.",
        license(name = "AGPL-3.0-or-later", identifier = "AGPL-3.0-or-later"),
    ),
    tags(
        (name = "meta", description = "What is answering"),
        (name = "nodes", description = "The nodes the instances see"),
        (name = "instances", description = "The Meshview instances, their history, and adding one"),
    ),
)]
struct Reference;

/// Handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub collector: Collector,
    pub limiter: Arc<RateLimiter>,
    pub config: Arc<Config>,
    pub cache: Arc<ResponseCache>,
}

impl AppState {
    pub fn new(pool: SqlitePool, collector: Collector, config: Config) -> Self {
        Self {
            pool,
            collector,
            limiter: Arc::new(RateLimiter::new(config.submissions.per_ip_per_hour)),
            config: Arc::new(config),
            cache: Arc::new(ResponseCache::default()),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let (api, reference) = OpenApiRouter::with_openapi(Reference::openapi())
        .routes(routes!(livez))
        .routes(routes!(version))
        .nest("/api/v1", v1::router())
        .split_for_parts();
    let spec = serde_json::to_string(&reference).expect("the reference serialises");

    // Any page may read; only this site's own pages may write, which they do
    // without asking.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::HEAD]);

    api.merge(legacy::router())
        .merge(Scalar::with_url(DOCS_PATH, reference).custom_html(SCALAR_HTML))
        .route(
            OPENAPI_PATH,
            get(move || {
                let spec = spec.clone();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        spec,
                    )
                }
            }),
        )
        .fallback(|| async { ApiError::not_found("Not found") })
        .layer(cors)
        .with_state(state)
}

/// Whether the process is alive.
#[derive(Serialize, ToSchema)]
pub struct Status {
    #[schema(example = "ok")]
    status: &'static str,
}

/// Liveness
///
/// Answers 200 for as long as the process can answer at all.
#[utoipa::path(
    get,
    path = "/livez",
    tag = "meta",
    responses((status = 200, description = "The process is running", body = Status))
)]
async fn livez() -> Json<Status> {
    Json(Status { status: "ok" })
}

/// Which build is answering.
#[derive(Debug, Serialize, ToSchema)]
pub struct Version {
    #[schema(example = "1.0.0")]
    pub version: &'static str,
    /// The date of the commit it was built from, or `unknown`.
    #[schema(example = "2026-11-13")]
    pub release_date: &'static str,
    /// The commit it was built from, or `unknown`.
    #[schema(example = "d8e2ce58c3948011ca08681f5227628562b64a81")]
    pub git_revision: &'static str,
    #[schema(example = "d8e2ce5")]
    pub git_revision_short: &'static str,
}

const REVISION: &str = env!("MESHVIEW_PLANET_GIT_REVISION");

pub fn current_version() -> Version {
    Version {
        version: env!("CARGO_PKG_VERSION"),
        release_date: env!("MESHVIEW_PLANET_GIT_DATE"),
        git_revision: REVISION,
        git_revision_short: if REVISION == "unknown" {
            REVISION
        } else {
            &REVISION[..REVISION.len().min(7)]
        },
    }
}

/// Version
///
/// The version answering, and the commit it was built from.
#[utoipa::path(
    get,
    path = "/version",
    tag = "meta",
    responses((status = 200, description = "The version", body = Version))
)]
async fn version() -> Json<Version> {
    Json(current_version())
}

/// Answers from the cache, building with `build` when the data changed.
async fn cached<F, Fut>(
    state: &AppState,
    key: String,
    headers: &HeaderMap,
    build: F,
) -> Result<Response, ApiError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<u8>, ApiError>>,
{
    state.cache.respond(&state.pool, key, headers, build).await
}

/// The UTC date `days` days ago.
fn days_back(days: i64) -> String {
    crate::db::utc_date_days_before(unix_now(), days)
}

/// The moment `days` days ago, in microseconds.
fn seen_since_us(days: i64) -> i64 {
    (unix_now() - days * 86_400) * 1_000_000
}

/// The query string, read leniently: a parameter given empty is one not given.
pub struct Params(HashMap<String, String>);

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(map) = Query::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?;
        Ok(Self(map))
    }
}

impl Params {
    fn text(&self, name: &str) -> Option<String> {
        self.0
            .get(name)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// A code, such as a country's, in upper case.
    fn code(&self, name: &str) -> Option<String> {
        self.text(name).map(|code| code.to_ascii_uppercase())
    }

    /// A number of days: `default` if not given, brought within 1 and `max`.
    fn days(&self, name: &str, default: i64, max: i64) -> Result<i64, ApiError> {
        match self.text(name) {
            None => Ok(default),
            Some(text) => text
                .parse::<i64>()
                .map(|days| days.clamp(1, max))
                .map_err(|_| ApiError::bad_request(format!("{name} has to be a whole number"))),
        }
    }

    fn boolean(&self, name: &str) -> Result<Option<bool>, ApiError> {
        match self.text(name).as_deref() {
            None => Ok(None),
            Some("true" | "1" | "yes") => Ok(Some(true)),
            Some("false" | "0" | "no") => Ok(Some(false)),
            Some(_) => Err(ApiError::bad_request(format!(
                "{name} has to be true or false"
            ))),
        }
    }
}

/// The client's address: the connection's, or with `behind_proxy`, the last
/// one in `X-Forwarded-For`, which is the one the proxy itself saw (those
/// before it are whatever the client chose to send).
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if state.config.server.behind_proxy
            && let Some(ip) = parts
                .headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(','))
                .filter_map(|entry| entry.trim().parse::<IpAddr>().ok())
                .next_back()
        {
            return Ok(Self(ip));
        }
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip());
        Ok(Self(ip))
    }
}
