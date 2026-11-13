// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The web server.

use crate::collector::Collector;
use crate::config::Config;
use crate::submit::RateLimiter;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::SqlitePool;
use std::sync::Arc;

/// Handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub collector: Collector,
    pub limiter: Arc<RateLimiter>,
    pub config: Arc<Config>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/livez", get(livez))
        .route("/version", get(version))
        .with_state(state)
}

#[derive(Serialize)]
struct Status {
    status: &'static str,
}

/// Answers for as long as the process can answer at all.
async fn livez() -> Json<Status> {
    Json(Status { status: "ok" })
}

/// Which build is answering.
#[derive(Debug, Serialize)]
pub struct Version {
    pub version: &'static str,
    pub release_date: &'static str,
    pub git_revision: &'static str,
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

async fn version() -> Json<Version> {
    Json(current_version())
}
