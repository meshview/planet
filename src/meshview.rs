// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Reading a Meshview instance: what it runs, and the nodes it sees.
//!
//! Every Meshview answers `/version`. The nodes are at `/api/v1/nodes` in the
//! Rust Meshview (5.x), which also redirects `/api/nodes` there, and at
//! `/api/nodes` in the Python one (3.x and 4.x) and in forks such as "plus".
//! All of them take `days_active` and answer `{"nodes": [...]}` with the same
//! fields, so `/api/v1/nodes` is tried first and `/api/nodes` when it is not
//! there.

use crate::db::{Node, Version};
use crate::net::{Client, FetchError};
use serde::Deserialize;
use serde_json::Value;
use std::fmt;

/// What an instance answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub version: Version,
    pub nodes: Vec<Node>,
}

/// Why an instance could not be read.
#[derive(Debug)]
pub enum ProbeError {
    /// A request failed.
    Fetch {
        what: &'static str,
        error: FetchError,
    },
    /// It answered, but not as a Meshview does.
    NotMeshview(String),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch { what, error } => write!(f, "{what}: {error}"),
            Self::NotMeshview(why) => write!(f, "does not answer like Meshview: {why}"),
        }
    }
}

impl std::error::Error for ProbeError {}

/// Reads an instance at `base`, a normalised URL.
pub async fn probe(client: &Client, base: &str, days_active: u32) -> Result<Snapshot, ProbeError> {
    let version = client
        .get(&format!("{base}/version"))
        .await
        .map_err(|error| ProbeError::Fetch {
            what: "/version",
            error,
        })?;
    let version = parse_version(&version).map_err(ProbeError::NotMeshview)?;

    let v1 = format!("{base}/api/v1/nodes?days_active={days_active}");
    let nodes = match client.get(&v1).await {
        // When something answers there but not as a Meshview (a page served
        // for any path, say), the older API may still be behind it.
        Ok(body) => parse_nodes(&body).ok(),
        // Not there: an older Meshview.
        Err(FetchError::Status(_)) => None,
        // Unreachable, or too slow: the older API would only be too.
        Err(error) => {
            return Err(ProbeError::Fetch {
                what: "/api/v1/nodes",
                error,
            });
        }
    };
    let nodes = match nodes {
        Some(nodes) => nodes,
        None => {
            let legacy = format!("{base}/api/nodes?days_active={days_active}");
            let body = client
                .get(&legacy)
                .await
                .map_err(|error| ProbeError::Fetch {
                    what: "/api/nodes",
                    error,
                })?;
            parse_nodes(&body).map_err(ProbeError::NotMeshview)?
        }
    };

    Ok(Snapshot { version, nodes })
}

#[derive(Deserialize)]
struct VersionBody {
    version: String,
    git_revision: Option<String>,
    git_revision_short: Option<String>,
    flavor: Option<String>,
}

/// Reads `/version`. Only `version` is required: forks answer just that.
pub fn parse_version(body: &[u8]) -> Result<Version, String> {
    let parsed: VersionBody =
        serde_json::from_slice(body).map_err(|_| not_json("/version", body))?;
    let version = clean(parsed.version, 64).ok_or("/version has no version")?;
    Ok(Version {
        version,
        git_revision: parsed.git_revision.and_then(|text| clean(text, 64)),
        git_revision_short: parsed.git_revision_short.and_then(|text| clean(text, 16)),
        flavor: parsed.flavor.and_then(|text| clean(text, 16)),
    })
}

#[derive(Deserialize)]
struct NodesBody {
    nodes: Vec<Value>,
}

/// Reads a nodes answer. A node without a usable `node_id` is left out, and so
/// is a repeated one; everything else about a node is optional.
pub fn parse_nodes(body: &[u8]) -> Result<Vec<Node>, String> {
    let parsed: NodesBody =
        serde_json::from_slice(body).map_err(|_| not_json("the nodes API", body))?;
    let mut seen = std::collections::HashSet::new();
    Ok(parsed
        .nodes
        .iter()
        .filter_map(parse_node)
        .filter(|node| seen.insert(node.node_id))
        .collect())
}

/// Says what came instead of the JSON expected. A page is worth telling apart:
/// it is often a bot check in front of the instance, which its admin has to
/// let this through.
fn not_json(what: &str, body: &[u8]) -> String {
    let start = body.iter().position(|b| !b.is_ascii_whitespace());
    if start.is_some_and(|i| body[i] == b'<') {
        format!("{what} answered a web page instead of JSON (a bot check in front of it?)")
    } else {
        format!("{what} did not answer the JSON expected")
    }
}

/// The longest any text from an instance is kept.
const TEXT_MAX: usize = 64;

fn parse_node(value: &Value) -> Option<Node> {
    let node_id = value.get("node_id")?.as_i64()?;
    if !(0..=i64::from(u32::MAX)).contains(&node_id) {
        return None;
    }
    let text = |key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .and_then(|text| clean(text.to_string(), TEXT_MAX))
    };
    let int = |key| value.get(key).and_then(Value::as_i64);

    Some(Node {
        node_id,
        long_name: text("long_name"),
        short_name: text("short_name"),
        hw_model: text("hw_model"),
        firmware: text("firmware"),
        role: text("role"),
        channel: text("channel"),
        position: position(int("last_lat"), int("last_long")),
        is_mqtt_gateway: value.get("is_mqtt_gateway").and_then(Value::as_bool),
        first_seen_us: int("first_seen_us"),
        last_seen_us: int("last_seen_us"),
    })
}

/// A position from Meshview's integers, degrees times ten million. 0,0 is what
/// Meshtastic sends for no position, and with the precision reduced it comes
/// as the middle of a square near it: within a degree of 0,0 is left out, as
/// Meshview's ingester does. Out of range is not a position either.
fn position(lat: Option<i64>, lon: Option<i64>) -> Option<(f64, f64)> {
    let (lat, lon) = (lat?, lon?);
    if lat.abs() <= 10_000_000 && lon.abs() <= 10_000_000 {
        return None;
    }
    let (lat, lon) = (lat as f64 / 1e7, lon as f64 / 1e7);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// Text as it is kept: without control characters, trimmed, cut to `max`
/// characters, and none when that leaves nothing.
fn clean(text: String, max: usize) -> Option<String> {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_of_every_kind_are_read() {
        let rust = br#"{"version":"1.0.0-dev","release_date":"2026-11-13","git_revision":"21714da68cd669122f18abbf798341dee3f32d1b","git_revision_short":"21714da","flavor":"rust"}"#;
        assert_eq!(
            parse_version(rust).unwrap(),
            Version {
                version: "1.0.0-dev".into(),
                git_revision: Some("21714da68cd669122f18abbf798341dee3f32d1b".into()),
                git_revision_short: Some("21714da".into()),
                flavor: Some("rust".into()),
            }
        );
        let plus = br#"{"version":"plus 1.0.0"}"#;
        assert_eq!(parse_version(plus).unwrap().version, "plus 1.0.0");
        assert_eq!(parse_version(plus).unwrap().flavor, None);
        assert!(parse_version(b"<html></html>").is_err());
        assert!(parse_version(br#"{"version":""}"#).is_err());
        assert!(parse_version(br#"{"name":"x"}"#).is_err());
    }

    #[test]
    fn nodes_are_read_as_meshview_answers_them() {
        let body = br#"{"nodes": [
            {"id": "!02ea6384", "node_id": 48915332, "long_name": "%Modulo Industries",
             "short_name": "%", "hw_model": "HELTEC_V3", "firmware": null,
             "role": "CLIENT_MUTE", "last_lat": 377367226, "last_long": -1224406628,
             "last_altitude": null, "channel": "MediumFast", "is_mqtt_gateway": null,
             "first_seen_us": 1772450863432839, "last_seen_us": 1794554095552815},
            {"node_id": 3677295964, "long_name": "No position", "last_lat": null,
             "last_long": null, "is_mqtt_gateway": true},
            {"node_id": 1, "last_lat": 0, "last_long": 0},
            {"node_id": 2, "last_lat": 2000000000, "last_long": 0},
            {"node_id": 3, "last_lat": 262144, "last_long": 262144},
            {"node_id": 4, "last_lat": 10000001, "last_long": 0},
            {"node_id": 48915332, "long_name": "A repeat"},
            {"id": "!no-number"},
            {"node_id": -5},
            "junk"
        ]}"#;
        let nodes = parse_nodes(body).unwrap();
        assert_eq!(nodes.len(), 6);
        assert_eq!(nodes[0].node_id, 48915332);
        assert_eq!(nodes[0].long_name.as_deref(), Some("%Modulo Industries"));
        assert_eq!(nodes[0].firmware, None);
        assert_eq!(nodes[0].position, Some((37.7367226, -122.4406628)));
        assert_eq!(nodes[0].last_seen_us, Some(1794554095552815));
        assert_eq!(nodes[1].position, None);
        assert_eq!(nodes[1].is_mqtt_gateway, Some(true));
        assert_eq!(nodes[2].position, None);
        assert_eq!(nodes[3].position, None);
        assert_eq!(nodes[4].position, None);
        assert_eq!(nodes[5].position, Some((1.0000001, 0.0)));
    }

    #[test]
    fn something_else_is_not_a_nodes_answer() {
        assert!(
            parse_nodes(b" <html></html>")
                .unwrap_err()
                .contains("web page")
        );
        assert!(parse_nodes(br#"{"instances": []}"#).is_err());
        assert!(parse_nodes(br#"{"nodes": []}"#).unwrap().is_empty());
    }

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean(" a\u{0}b\n ".into(), 10).as_deref(), Some("ab"));
        assert_eq!(clean("\u{7}".into(), 10), None);
        assert_eq!(clean("abcdef".into(), 3).as_deref(), Some("abc"));
    }
}
