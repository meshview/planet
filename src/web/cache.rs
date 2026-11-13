// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Answers kept ready, serialised and compressed.
//!
//! What the web shows only changes when the collector, a submission or the
//! cleanup writes, which is a few times an hour at most, while the map's
//! answer is megabytes of JSON. So each answer is built once per change of
//! the database's [fingerprint](crate::db::fingerprint), kept both as it is
//! and gzipped, and served from memory until the next change. Its ETag is
//! the fingerprint, so a browser that already has it gets a 304 and nothing
//! else.

use super::error::ApiError;
use crate::db;
use axum::http::header::{
    ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE, ETAG, IF_NONE_MATCH, VARY,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use flate2::Compression;
use flate2::write::GzEncoder;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long an answer is served after the data changed before it is built
/// again. A collection round writes once per instance, so the data changes
/// dozens of times in a minute; rebuilding the map's answer on each would be
/// work thrown away. A few seconds behind is nothing for data collected hours
/// apart.
pub const MIN_AGE: Duration = Duration::from_secs(10);

/// More different answers than this, and the oldest are all dropped: there
/// are a handful of pages, and a client asking for endless variations should
/// not grow the memory with them.
const MAX_ENTRIES: usize = 256;

/// An answer, ready to send.
#[derive(Debug)]
pub struct Entry {
    built_at: Instant,
    etag: HeaderValue,
    json: Bytes,
    gzip: Bytes,
}

impl Entry {
    /// Serialises and compresses a body, tagged with `tag`.
    pub fn new(json: Vec<u8>, tag: &str) -> Self {
        let mut encoder = GzEncoder::new(Vec::with_capacity(json.len() / 6), Compression::new(6));
        encoder.write_all(&json).expect("writing to memory");
        let gzip = encoder.finish().expect("writing to memory");
        Self {
            built_at: Instant::now(),
            etag: HeaderValue::from_str(&format!("\"{tag}\"")).expect("a hex tag"),
            json: Bytes::from(json),
            gzip: Bytes::from(gzip),
        }
    }

    /// The answer for a request with these headers: a 304 if it already has
    /// this, else the body, gzipped if it takes gzip.
    pub fn respond(&self, request: &HeaderMap) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(ETAG, self.etag.clone());
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));

        let matches = request
            .get(IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|tag| tag.trim().trim_start_matches("W/") == self.etag)
            });
        if matches {
            return (StatusCode::NOT_MODIFIED, headers).into_response();
        }

        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if accepts_gzip(request) {
            headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            (headers, self.gzip.clone()).into_response()
        } else {
            (headers, self.json.clone()).into_response()
        }
    }
}

fn accepts_gzip(request: &HeaderMap) -> bool {
    request
        .get_all(ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|coding| {
            let mut parts = coding.split(';');
            let name = parts.next().unwrap_or_default().trim();
            let refused = parts.any(|param| {
                let param = param.trim().replace(' ', "");
                param == "q=0" || param == "q=0.0" || param == "q=0.00" || param == "q=0.000"
            });
            (name.eq_ignore_ascii_case("gzip") || name == "*") && !refused
        })
}

/// The answers, by what was asked.
#[derive(Debug)]
pub struct ResponseCache {
    min_age: Duration,
    entries: Mutex<HashMap<String, (String, Arc<Entry>)>>,
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::with_min_age(MIN_AGE)
    }
}

impl ResponseCache {
    /// A cache that serves an answer for at least `min_age` after it is built,
    /// even if the data changed: see [`MIN_AGE`].
    pub fn with_min_age(min_age: Duration) -> Self {
        Self {
            min_age,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Forgets every answer, for when this process itself changed the data
    /// and whoever changed it expects to see it at once.
    pub fn clear(&self) {
        self.entries.lock().expect("not poisoned").clear();
    }

    /// Answers `key` from the cache if the database has not changed since it
    /// was built, or if it was built only moments ago, and builds it with
    /// `build` otherwise. An answer served after the data changed keeps its
    /// own tag, so a client is never told it is up to date with a tag that
    /// does not match what it has.
    ///
    /// `key` has to say everything the answer depends on besides the
    /// database: the route and its parameters. The current UTC date is added
    /// here, since the history and the trends are counted in days.
    pub async fn respond<F, Fut>(
        &self,
        pool: &SqlitePool,
        key: String,
        request: &HeaderMap,
        build: F,
    ) -> Result<Response, ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<u8>, ApiError>>,
    {
        let fingerprint = format!(
            "{}/{}",
            db::fingerprint(pool).await?,
            db::utc_date(crate::collector::unix_now())
        );
        let tag = {
            let mut hasher = DefaultHasher::new();
            fingerprint.hash(&mut hasher);
            key.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };

        // Asked with the tag it already has, there is nothing to build.
        let current = HeaderValue::from_str(&format!("\"{tag}\"")).expect("a hex tag");
        if request
            .get(IF_NONE_MATCH)
            .is_some_and(|value| value == current)
        {
            let mut headers = HeaderMap::new();
            headers.insert(ETAG, current);
            headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));
            return Ok((StatusCode::NOT_MODIFIED, headers).into_response());
        }

        let cached = self
            .entries
            .lock()
            .expect("not poisoned")
            .get(&key)
            .filter(|(built_for, entry)| {
                *built_for == fingerprint || entry.built_at.elapsed() < self.min_age
            })
            .map(|(_, entry)| entry.clone());
        let entry = match cached {
            Some(entry) => entry,
            None => {
                let json = build().await?;
                let entry = Arc::new(Entry::new(json, &tag));
                let mut entries = self.entries.lock().expect("not poisoned");
                if entries.len() >= MAX_ENTRIES {
                    entries.clear();
                }
                entries.insert(key, (fingerprint, entry.clone()));
                entry
            }
        };
        Ok(entry.respond(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| (name.parse().unwrap(), HeaderValue::from_static(value)))
            .collect()
    }

    #[test]
    fn gzip_is_sent_only_to_those_that_take_it() {
        assert!(accepts_gzip(&headers(&[(
            "accept-encoding",
            "gzip, deflate, br"
        )])));
        assert!(accepts_gzip(&headers(&[(
            "accept-encoding",
            "br;q=1.0, gzip;q=0.8"
        )])));
        assert!(accepts_gzip(&headers(&[("accept-encoding", "*")])));
        assert!(!accepts_gzip(&headers(&[("accept-encoding", "gzip;q=0")])));
        assert!(!accepts_gzip(&headers(&[("accept-encoding", "identity")])));
        assert!(!accepts_gzip(&HeaderMap::new()));
    }

    #[test]
    fn an_entry_answers_304_to_its_own_tag() {
        let entry = Entry::new(b"{\"a\":1}".to_vec(), "abc");
        let fresh = entry.respond(&HeaderMap::new());
        assert_eq!(fresh.status(), StatusCode::OK);
        assert_eq!(fresh.headers()[ETAG], "\"abc\"");
        let again = entry.respond(&headers(&[("if-none-match", "W/\"abc\"")]));
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        let other = entry.respond(&headers(&[("if-none-match", "\"xyz\"")]));
        assert_eq!(other.status(), StatusCode::OK);
    }
}
