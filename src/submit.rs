// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Adding an instance from the web.
//!
//! There is no moderation, so what is accepted is only what cannot be abused:
//! a URL that answers as a Meshview does, right now, and an area picked from
//! the closed list. Nothing the submitter types is ever shown as text. Each
//! address gets a few attempts an hour, accepted or not, which is what keeps
//! the server from being used to probe the world with.
//!
//! An accepted instance is published at once, with the nodes it answered.

use crate::areas::{AreaError, Areas};
use crate::collector::Collector;
use crate::db;
use crate::meshview::{self, ProbeError};
use crate::net::{UrlError, normalise_url};
use sqlx::SqlitePool;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(3600);

/// Counts the attempts of each address in the latest hour.
///
/// IPv6 addresses count by their /64, which is what a single host usually
/// gets: counting each address would let it rotate through them.
#[derive(Debug)]
pub struct RateLimiter {
    per_hour: u32,
    attempts: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new(per_hour: u32) -> Self {
        Self {
            per_hour,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Counts an attempt, and answers whether it is within the limit.
    pub fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let key = bucket(ip);
        let mut attempts = self.attempts.lock().expect("not poisoned");
        // Forget the addresses whose attempts are all old, so that the map does
        // not grow for ever.
        attempts.retain(|_, times| {
            while times
                .front()
                .is_some_and(|&time| now.duration_since(time) >= WINDOW)
            {
                times.pop_front();
            }
            !times.is_empty()
        });
        let times = attempts.entry(key).or_default();
        if times.len() >= self.per_hour as usize {
            return false;
        }
        times.push_back(now);
        true
    }
}

/// What an address is counted as.
fn bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let mut segments = v6.segments();
                segments[4..].fill(0);
                IpAddr::V6(segments.into())
            }
        },
    }
}

/// An instance someone wants added.
#[derive(Debug, Clone)]
pub struct Submission {
    pub url: String,
    pub country: String,
    pub region: Option<String>,
}

/// An instance added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub id: i64,
    pub url: String,
    pub version: String,
    pub nodes: usize,
}

/// Why a submission was not accepted.
#[derive(Debug)]
pub enum Rejection {
    /// Submissions are turned off.
    Disabled,
    /// Too many attempts from this address.
    RateLimited,
    Url(UrlError),
    Area(AreaError),
    /// The URL is already listed.
    Duplicate,
    /// It did not answer as a Meshview.
    Probe(ProbeError),
    /// Something on this side failed.
    Internal(anyhow::Error),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("new instances are not being accepted"),
            Self::RateLimited => f.write_str("too many submissions; try again later"),
            Self::Url(e) => write!(f, "URL: {e}"),
            Self::Area(e) => write!(f, "area: {e}"),
            Self::Duplicate => f.write_str("this instance is already listed"),
            Self::Probe(e) => write!(f, "the instance {e}"),
            Self::Internal(e) => write!(f, "internal error: {e:#}"),
        }
    }
}

impl std::error::Error for Rejection {}

/// Checks a submission and, if it holds, adds the instance with what it
/// answered.
pub async fn submit(
    pool: &SqlitePool,
    collector: &Collector,
    submission: &Submission,
    now: i64,
) -> Result<Accepted, Rejection> {
    let allow_private = collector.settings().allow_private_addresses;
    let url = normalise_url(&submission.url, allow_private).map_err(Rejection::Url)?;
    let (country, region) = Areas::get()
        .check(&submission.country, submission.region.as_deref())
        .map_err(Rejection::Area)?;
    if db::instance_by_url(pool, &url)
        .await
        .map_err(Rejection::Internal)?
        .is_some()
    {
        return Err(Rejection::Duplicate);
    }

    let snapshot = meshview::probe(collector.client(), &url, collector.settings().days_active)
        .await
        .map_err(Rejection::Probe)?;

    // Checked again by the insert itself: two submissions of one URL at once
    // both get past the check above.
    let id = db::add_instance(pool, &url, &country, region.as_deref(), now)
        .await
        .map_err(Rejection::Internal)?
        .ok_or(Rejection::Duplicate)?;
    db::record_success(pool, id, now, &snapshot.version, &snapshot.nodes)
        .await
        .map_err(Rejection::Internal)?;

    Ok(Accepted {
        id,
        url,
        version: snapshot.version.version,
        nodes: snapshot.nodes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_is_per_address_per_hour() {
        let limiter = RateLimiter::new(2);
        let start = Instant::now();
        let a: IpAddr = "203.0.113.1".parse().unwrap();
        let b: IpAddr = "203.0.113.2".parse().unwrap();
        assert!(limiter.allow(a, start));
        assert!(limiter.allow(a, start));
        assert!(!limiter.allow(a, start));
        assert!(limiter.allow(b, start));
        // An hour after the first two, they no longer count.
        assert!(limiter.allow(a, start + WINDOW));
    }

    #[test]
    fn ipv6_counts_by_its_64() {
        let limiter = RateLimiter::new(1);
        let now = Instant::now();
        assert!(limiter.allow("2001:db8:1:2::1".parse().unwrap(), now));
        assert!(!limiter.allow("2001:db8:1:2:ffff::9".parse().unwrap(), now));
        assert!(limiter.allow("2001:db8:1:3::1".parse().unwrap(), now));
    }
}
