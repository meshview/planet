// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Errors from the API, always as JSON: `{"error": "...", "code": "..."}`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tracing::error;
use utoipa::ToSchema;

/// The body of every error the API answers.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// What was wrong, in English, for a person.
    #[schema(example = "\"XX\" is not a known country")]
    pub error: String,
    /// What was wrong, for a program: `bad_request`, `invalid_url`,
    /// `private_address`, `area_required`, `unknown_country`, `unknown_region`,
    /// `duplicate`, `not_meshview`, `rate_limited`, `disabled`, `not_found` or
    /// `internal`.
    #[schema(example = "unknown_country")]
    pub code: &'static str,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// A failure on this side. What went wrong is logged; the client is only
    /// told that something did.
    pub fn internal(cause: &anyhow::Error) -> Self {
        error!("{cause:#}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Something went wrong on the server",
        )
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(cause: anyhow::Error) -> Self {
        Self::internal(&cause)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
                code: self.code,
            }),
        )
            .into_response()
    }
}
