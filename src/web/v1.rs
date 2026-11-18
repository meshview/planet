// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The documented API, under `/api/v1`.

use super::error::{ApiError, ErrorBody};
use super::{AppState, ClientIp, Params, cached, days_back, seen_since_us};
use crate::areas::{Areas, Language};
use crate::collector::unix_now;
use crate::db::{self, InstanceSummary, NodeFilter};
use crate::submit::{self, Rejection, Submission};
use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// The days of history the trend of each instance covers.
pub const TREND_DAYS: i64 = 30;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(nodes))
        .routes(routes!(instances, add_instance))
        .routes(routes!(histogram))
        .routes(routes!(areas))
}

/// A node, as one instance sees it.
#[derive(Debug, Serialize, ToSchema)]
pub struct Node {
    /// Its number.
    #[schema(example = 48915332)]
    pub node_id: i64,
    /// Its number as Meshtastic writes it.
    #[schema(example = "!02ea6384")]
    pub id: String,
    #[schema(example = "Modulo Industries")]
    pub long_name: Option<String>,
    #[schema(example = "MI")]
    pub short_name: Option<String>,
    #[schema(example = "HELTEC_V3")]
    pub hw_model: Option<String>,
    #[schema(example = "2.7.15")]
    pub firmware: Option<String>,
    #[schema(example = "CLIENT")]
    pub role: Option<String>,
    /// The channel it was last heard on.
    #[schema(example = "MediumFast")]
    pub channel: Option<String>,
    /// Latitude in degrees, null without a position.
    #[schema(example = 37.7367226)]
    pub lat: Option<f64>,
    /// Longitude in degrees, null without a position.
    #[schema(example = -122.4406628)]
    pub lon: Option<f64>,
    pub is_mqtt_gateway: Option<bool>,
    /// When the instance first heard it, in microseconds.
    #[schema(example = 1772450863432839_i64)]
    pub first_seen_us: Option<i64>,
    /// When the instance last heard it, in microseconds.
    #[schema(example = 1794554095552815_i64)]
    pub last_seen_us: Option<i64>,
    /// The instance that sees it.
    #[schema(example = "https://meshview.bayme.sh")]
    pub instance_url: String,
}

/// Nodes, and how many there are.
#[derive(Debug, Serialize, ToSchema)]
pub struct Nodes {
    pub nodes: Vec<Node>,
    /// The nodes matching, with a position or not, whatever `has_position`.
    #[schema(example = 19199)]
    pub total: usize,
    /// Of those, the ones with a position.
    #[schema(example = 12724)]
    pub mapped: usize,
}

/// Nodes
///
/// The nodes the online instances see. A node seen by two instances is there
/// twice, once for each. The answer changes only when an instance is
/// collected, so it carries an ETag: ask with `If-None-Match` and an unchanged
/// answer is a 304 with no body.
#[utoipa::path(
    get,
    path = "/nodes",
    tag = "nodes",
    params(
        ("days_active" = Option<i64>, Query, description = "Only nodes heard in this many days, 1 to 365. Default 14."),
        ("has_position" = Option<bool>, Query, description = "Only nodes with a position (`true`), or only those without (`false`). Default: both."),
        ("country" = Option<String>, Query, description = "Only nodes of instances in this country, ISO 3166-1 alpha-2."),
        ("region" = Option<String>, Query, description = "Only nodes of instances in this region, ISO 3166-2."),
        ("channel" = Option<String>, Query, description = "Only nodes last heard on this channel."),
        ("instance" = Option<String>, Query, description = "Only nodes of the instance at this URL."),
    ),
    responses(
        (status = 200, description = "The nodes", body = Nodes),
        (status = 304, description = "Unchanged since the ETag given"),
        (status = 400, description = "A parameter is not valid", body = ErrorBody),
    )
)]
pub async fn nodes(
    State(state): State<AppState>,
    params: Params,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let days = params.days("days_active", 14, 365)?;
    let has_position = params.boolean("has_position")?;
    let filter = NodeFilter {
        seen_since_us: 0,
        country: params.code("country"),
        region: params.code("region"),
        channel: params.text("channel"),
        instance_url: params
            .text("instance")
            .map(|url| crate::net::normalise_url(&url, true).unwrap_or(url)),
    };
    let key = format!("v1/nodes {days} {has_position:?} {filter:?}");
    cached(&state, key, &headers, || async {
        let filter = NodeFilter {
            seen_since_us: seen_since_us(days),
            ..filter.clone()
        };
        let rows = db::nodes(&state.pool, &filter).await?;
        let total = rows.len();
        let mapped = rows.iter().filter(|row| row.lat.is_some()).count();
        let nodes = rows
            .into_iter()
            .filter(|row| has_position.is_none_or(|wanted| row.lat.is_some() == wanted))
            .map(|row| Node {
                id: format!("!{:08x}", row.node_id),
                node_id: row.node_id,
                long_name: row.long_name,
                short_name: row.short_name,
                hw_model: row.hw_model,
                firmware: row.firmware,
                role: row.role,
                channel: row.channel,
                lat: row.lat,
                lon: row.lon,
                is_mqtt_gateway: row.is_mqtt_gateway,
                first_seen_us: row.first_seen_us,
                last_seen_us: row.last_seen_us,
                instance_url: row.instance_url,
            })
            .collect();
        Ok(serde_json::to_vec(&Nodes {
            nodes,
            total,
            mapped,
        })
        .expect("nodes serialise"))
    })
    .await
}

/// An area's name in each language.
#[derive(Debug, Serialize, ToSchema)]
pub struct AreaNames {
    #[schema(example = "Galicia, Spain")]
    pub en: String,
    #[schema(example = "Galicia, España")]
    pub es: String,
}

impl AreaNames {
    pub fn of(country: &str, region: Option<&str>) -> Self {
        let areas = Areas::get();
        Self {
            en: areas.label(country, region, Language::English),
            es: areas.label(country, region, Language::Spanish),
        }
    }
}

/// How an instance's node count moved.
#[derive(Debug, Serialize, ToSchema)]
pub struct Trend {
    /// The first daily count in the period.
    #[schema(example = 1446)]
    pub start_count: i64,
    /// The latest.
    #[schema(example = 1518)]
    pub end_count: i64,
}

impl Trend {
    pub fn of(summary: &InstanceSummary) -> Self {
        Self {
            start_count: summary.trend_start.unwrap_or(summary.node_count),
            end_count: summary.trend_end.unwrap_or(summary.node_count),
        }
    }
}

/// Whether an instance answers.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// It answered in the latest days; its nodes are on the map.
    Online,
    /// It has not answered for days; its nodes are kept off the map, and it is
    /// deleted if it stays so.
    Offline,
}

/// An instance.
#[derive(Debug, Serialize, ToSchema)]
pub struct Instance {
    #[schema(example = "https://meshview.mesh.comunidadeozulo.org")]
    pub url: String,
    /// ISO 3166-1 alpha-2.
    #[schema(example = "ES")]
    pub country: String,
    /// ISO 3166-2, or null for the whole country.
    #[schema(example = "ES-GA")]
    pub region: Option<String>,
    /// The area's name.
    pub area: AreaNames,
    pub status: Status,
    /// What it runs, from its `/version`.
    #[schema(example = "5.0.0")]
    pub version: Option<String>,
    #[schema(example = "21714da68cd669122f18abbf798341dee3f32d1b")]
    pub git_revision: Option<String>,
    #[schema(example = "21714da")]
    pub git_revision_short: Option<String>,
    /// Which Meshview it is, as its `/version` says: `rust`, or null when it
    /// does not say, as the Python one.
    #[schema(example = "rust")]
    pub flavor: Option<String>,
    /// The nodes of its latest good collection.
    #[schema(example = 1283)]
    pub node_count: i64,
    /// Of those, the ones with a position.
    #[schema(example = 940)]
    pub mapped_count: i64,
    /// Over the latest 30 days.
    pub trend_30d: Trend,
    /// When it was added, in unix seconds.
    #[schema(example = 1794571200)]
    pub added_at: i64,
    /// When it last answered, in unix seconds.
    #[schema(example = 1794571200)]
    pub last_success_at: Option<i64>,
    /// When it was last asked, in unix seconds.
    #[schema(example = 1794571200)]
    pub last_attempt_at: Option<i64>,
    /// Why the latest collection failed; null when it did not.
    #[schema(example = json!(null))]
    pub last_error: Option<String>,
    /// Since when it is offline, in unix seconds.
    #[schema(example = json!(null))]
    pub offline_since: Option<i64>,
}

impl From<InstanceSummary> for Instance {
    fn from(summary: InstanceSummary) -> Self {
        let trend_30d = Trend::of(&summary);
        let i = summary.instance;
        Self {
            area: AreaNames::of(&i.country, i.region.as_deref()),
            status: if i.offline_since.is_some() {
                Status::Offline
            } else {
                Status::Online
            },
            url: i.url,
            country: i.country,
            region: i.region,
            version: i.version,
            git_revision: i.git_revision,
            git_revision_short: i.git_revision_short,
            flavor: i.flavor,
            node_count: summary.node_count,
            mapped_count: summary.mapped_count,
            trend_30d,
            added_at: i.added_at,
            last_success_at: i.last_success_at,
            last_attempt_at: i.last_attempt_at,
            last_error: i.last_error,
            offline_since: i.offline_since,
        }
    }
}

/// The instances.
#[derive(Debug, Serialize, ToSchema)]
pub struct Instances {
    pub instances: Vec<Instance>,
}

/// Instances
///
/// Every instance listed: those online first, then by nodes. Carries an ETag,
/// as `/nodes`.
#[utoipa::path(
    get,
    path = "/instances",
    tag = "instances",
    responses(
        (status = 200, description = "The instances", body = Instances),
        (status = 304, description = "Unchanged since the ETag given"),
    )
)]
pub async fn instances(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    cached(&state, "v1/instances".into(), &headers, || async {
        let since = days_back(TREND_DAYS - 1);
        let instances = db::instance_summaries(&state.pool, &since)
            .await?
            .into_iter()
            .map(Instance::from)
            .collect();
        Ok(serde_json::to_vec(&Instances { instances }).expect("instances serialise"))
    })
    .await
}

/// An instance to add.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NewInstance {
    /// Its address; `https://` is assumed without a scheme.
    #[schema(example = "https://meshview.example.org")]
    pub url: String,
    /// ISO 3166-1 alpha-2, from `/areas`.
    #[schema(example = "ES")]
    pub country: Option<String>,
    /// ISO 3166-2, from `/areas`, or nothing for the whole country.
    #[schema(example = "ES-GA")]
    pub region: Option<String>,
    /// The old site's free text area. Not accepted any more: sending it
    /// without `country` is an error that says so.
    #[schema(ignore)]
    #[serde(default)]
    pub name: Option<String>,
}

/// An instance just added.
#[derive(Debug, Serialize, ToSchema)]
pub struct Added {
    #[schema(example = "https://meshview.example.org")]
    pub url: String,
    #[schema(example = "ES")]
    pub country: String,
    #[schema(example = "ES-GA")]
    pub region: Option<String>,
    pub area: AreaNames,
    /// What it runs.
    #[schema(example = "5.0.0")]
    pub version: String,
    /// The nodes it answered, already on the map.
    #[schema(example = 812)]
    pub node_count: usize,
}

/// Add an instance
///
/// Adds a Meshview instance, which is published at once. It has to answer as
/// a Meshview there and then (its `/version` and its nodes), and its area is a
/// country and optionally one of its regions from `/areas`. Each address may
/// try a few times an hour, whether the attempts are accepted or not.
#[utoipa::path(
    post,
    path = "/instances",
    tag = "instances",
    request_body = NewInstance,
    responses(
        (status = 201, description = "Added and published", body = Added),
        (status = 400, description = "The URL or the area is not valid (`invalid_url`, `private_address`, `area_required`, `unknown_country`, `unknown_region`, `bad_request`)", body = ErrorBody),
        (status = 403, description = "Instances are not being accepted (`disabled`), or not this one, which the admin has disabled (`blocked`)", body = ErrorBody),
        (status = 409, description = "Already listed (`duplicate`)", body = ErrorBody),
        (status = 422, description = "It does not answer as a Meshview (`not_meshview`)", body = ErrorBody),
        (status = 429, description = "Too many attempts from this address (`rate_limited`)", body = ErrorBody),
    )
)]
pub async fn add_instance(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    body: Result<Json<NewInstance>, JsonRejection>,
) -> Result<Response, ApiError> {
    if !state.config.submissions.enabled {
        return Err(rejection(Rejection::Disabled));
    }
    if !state.limiter.allow(ip, std::time::Instant::now()) {
        return Err(rejection(Rejection::RateLimited));
    }
    let Json(body) = body.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let Some(country) = body.country.filter(|country| !country.trim().is_empty()) else {
        let message = if body.name.is_some() {
            "Free text area names are not accepted any more: send `country` (ISO 3166-1) and \
             optionally `region` (ISO 3166-2), from /api/v1/areas"
        } else {
            "`country` is required (ISO 3166-1), and optionally `region` (ISO 3166-2), from \
             /api/v1/areas"
        };
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "area_required",
            message,
        ));
    };
    let submission = Submission {
        url: body.url,
        country,
        region: body.region,
    };
    let accepted = submit::submit(&state.pool, &state.collector, &submission, unix_now())
        .await
        .map_err(rejection)?;
    let instance = db::instance_by_url(&state.pool, &accepted.url)
        .await?
        .ok_or_else(|| anyhow::anyhow!("{} vanished right after being added", accepted.url))?;
    state.cache.clear();
    tracing::info!("{} added from {ip}", accepted.url);
    state
        .collector
        .notifier()
        .send(crate::notify::Notice::added(
            &accepted.url,
            &instance.country,
            instance.region.as_deref(),
            &accepted.version,
            accepted.nodes,
            ip,
            unix_now(),
        ));
    Ok((
        StatusCode::CREATED,
        Json(Added {
            area: AreaNames::of(&instance.country, instance.region.as_deref()),
            url: accepted.url,
            country: instance.country,
            region: instance.region,
            version: accepted.version,
            node_count: accepted.nodes,
        }),
    )
        .into_response())
}

/// A rejected submission as the error the API answers.
pub fn rejection(rejection: Rejection) -> ApiError {
    use crate::areas::AreaError;
    use crate::net::UrlError;
    let message = rejection.to_string();
    let (status, code) = match &rejection {
        Rejection::Disabled => (StatusCode::FORBIDDEN, "disabled"),
        Rejection::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        Rejection::Url(UrlError::Private) => (StatusCode::BAD_REQUEST, "private_address"),
        Rejection::Url(_) => (StatusCode::BAD_REQUEST, "invalid_url"),
        Rejection::Area(AreaError::UnknownCountry(_)) => {
            (StatusCode::BAD_REQUEST, "unknown_country")
        }
        Rejection::Area(AreaError::UnknownRegion { .. }) => {
            (StatusCode::BAD_REQUEST, "unknown_region")
        }
        Rejection::Duplicate => (StatusCode::CONFLICT, "duplicate"),
        Rejection::Blocked => (StatusCode::FORBIDDEN, "blocked"),
        Rejection::Probe(_) => (StatusCode::UNPROCESSABLE_ENTITY, "not_meshview"),
        Rejection::Internal(e) => return ApiError::internal(e),
    };
    ApiError::new(status, code, message)
}

/// A daily count.
#[derive(Debug, Serialize, ToSchema)]
pub struct Point {
    #[schema(example = "2026-11-13")]
    pub date: String,
    #[schema(example = 1283)]
    pub node_count: i64,
    /// When it was taken, in unix seconds: the day's latest good collection.
    #[schema(example = 1794571200)]
    pub recorded_at: i64,
}

/// An instance's daily counts.
#[derive(Debug, Serialize, ToSchema)]
pub struct Series {
    #[schema(example = "https://meshview.mesh.comunidadeozulo.org")]
    pub url: String,
    #[schema(example = "ES")]
    pub country: String,
    #[schema(example = "ES-GA")]
    pub region: Option<String>,
    pub area: AreaNames,
    pub points: Vec<Point>,
}

/// Daily counts of every instance.
#[derive(Debug, Serialize, ToSchema)]
pub struct Histogram {
    #[schema(example = 30)]
    pub days: i64,
    /// The first day, UTC.
    #[schema(example = "2026-10-15")]
    pub start_date: String,
    /// Today, UTC.
    #[schema(example = "2026-11-13")]
    pub end_date: String,
    /// The instances with a count in those days.
    pub instances: Vec<Series>,
}

/// Daily counts as both versions of the API answer them, from one read.
pub struct HistogramData {
    pub days: i64,
    pub start_date: String,
    pub end_date: String,
    /// (instance, points), the instances in the list's order.
    pub series: Vec<(db::Instance, Vec<db::DailyRow>)>,
}

pub async fn histogram_data(state: &AppState, days: i64) -> Result<HistogramData, ApiError> {
    let start_date = days_back(days - 1);
    let end_date = days_back(0);
    let summaries = db::instance_summaries(&state.pool, &start_date).await?;
    let mut by_instance: BTreeMap<i64, Vec<db::DailyRow>> = BTreeMap::new();
    for row in db::daily_between(&state.pool, &start_date, &end_date).await? {
        by_instance.entry(row.instance_id).or_default().push(row);
    }
    let series = summaries
        .into_iter()
        .filter_map(|summary| {
            let points = by_instance.remove(&summary.instance.id)?;
            Some((summary.instance, points))
        })
        .collect();
    Ok(HistogramData {
        days,
        start_date,
        end_date,
        series,
    })
}

/// History
///
/// How many nodes each instance had, day by day (UTC): the count of the
/// day's latest good collection. Days it did not answer have no count.
#[utoipa::path(
    get,
    path = "/histogram",
    tag = "instances",
    params(
        ("days" = Option<i64>, Query, description = "How many days, today included, 1 to 366. Default 30."),
    ),
    responses(
        (status = 200, description = "The counts", body = Histogram),
        (status = 304, description = "Unchanged since the ETag given"),
        (status = 400, description = "`days` is not a number", body = ErrorBody),
    )
)]
pub async fn histogram(
    State(state): State<AppState>,
    params: Params,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let days = params.days("days", 30, 366)?;
    cached(&state, format!("v1/histogram {days}"), &headers, || async {
        let data = histogram_data(&state, days).await?;
        let instances = data
            .series
            .into_iter()
            .map(|(instance, points)| Series {
                area: AreaNames::of(&instance.country, instance.region.as_deref()),
                url: instance.url,
                country: instance.country,
                region: instance.region,
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
    })
    .await
}

/// A region to choose.
#[derive(Debug, Serialize, ToSchema)]
pub struct RegionChoice {
    #[schema(example = "ES-GA")]
    pub code: String,
    /// Its ISO name, the local one.
    #[schema(example = "Galicia")]
    pub name: String,
}

/// A country to choose, with its regions.
#[derive(Debug, Serialize, ToSchema)]
pub struct CountryChoice {
    #[schema(example = "ES")]
    pub code: String,
    #[schema(example = "Spain")]
    pub name: String,
    pub regions: Vec<RegionChoice>,
}

/// The areas an instance can be in.
#[derive(Debug, Serialize, ToSchema)]
pub struct AreaList {
    /// Where the list comes from.
    #[schema(example = "iso-codes 4.20.1")]
    pub source: String,
    /// By name, in the language asked for.
    pub countries: Vec<CountryChoice>,
}

/// Areas
///
/// The countries (ISO 3166-1) and their top level regions (ISO 3166-2) an
/// instance's area is chosen from, by name. Country names are in the language
/// asked for; region names are their ISO ones, the local ones.
#[utoipa::path(
    get,
    path = "/areas",
    tag = "instances",
    params(
        ("lang" = Option<String>, Query, description = "`en` or `es`. Default `en`."),
    ),
    responses(
        (status = 200, description = "The areas", body = AreaList),
        (status = 304, description = "Unchanged since the ETag given"),
        (status = 400, description = "Not a language there are names in", body = ErrorBody),
    )
)]
pub async fn areas(params: Params, headers: HeaderMap) -> Result<Response, ApiError> {
    static ENGLISH: OnceLock<super::cache::Entry> = OnceLock::new();
    static SPANISH: OnceLock<super::cache::Entry> = OnceLock::new();
    let (language, cell) = match params.text("lang").as_deref() {
        None | Some("en") => (Language::English, &ENGLISH),
        Some("es") => (Language::Spanish, &SPANISH),
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "there are no names in {other:?}: en or es"
            )));
        }
    };
    let entry = cell.get_or_init(|| {
        let list = area_list(language);
        super::cache::Entry::new(
            serde_json::to_vec(&list).expect("areas serialise"),
            &format!(
                "areas-{}-{}",
                language.code(),
                list.source.replace(' ', "-")
            ),
        )
    });
    Ok(entry.respond(&headers))
}

fn area_list(language: Language) -> AreaList {
    let areas = Areas::get();
    let mut countries: Vec<CountryChoice> = areas
        .countries()
        .iter()
        .map(|country| {
            let mut regions: Vec<RegionChoice> = country
                .regions
                .iter()
                .map(|region| RegionChoice {
                    code: region.code.clone(),
                    name: region.name.clone(),
                })
                .collect();
            regions.sort_by_cached_key(|region| sort_key(&region.name));
            CountryChoice {
                code: country.code.clone(),
                name: country.name(language).to_string(),
                regions,
            }
        })
        .collect();
    countries.sort_by_cached_key(|country| sort_key(&country.name));
    AreaList {
        source: areas.source().to_string(),
        countries,
    }
}

/// What a name sorts by: lower case, and Latin letters without their accents,
/// so that "Åland" goes with the A and "Ísland" with the I.
fn sort_key(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'č' => 'c',
            'ď' | 'đ' => 'd',
            'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => 'i',
            'ł' | 'ľ' | 'ĺ' => 'l',
            'ñ' | 'ń' | 'ň' => 'n',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' | 'ő' => 'o',
            'ř' | 'ŕ' => 'r',
            'ś' | 'š' | 'ş' | 'ș' => 's',
            'ť' | 'ţ' | 'ț' => 't',
            'ú' | 'ù' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => 'u',
            'ý' | 'ÿ' => 'y',
            'ź' | 'ż' | 'ž' => 'z',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_as_a_reader_expects() {
        let list = area_list(Language::Spanish);
        let names: Vec<&str> = list.countries.iter().map(|c| c.name.as_str()).collect();
        let position = |name| names.iter().position(|n| *n == name).unwrap();
        assert!(position("España") < position("Estados Unidos"));
        let spain = list.countries.iter().find(|c| c.code == "ES").unwrap();
        let regions: Vec<&str> = spain.regions.iter().map(|r| r.name.as_str()).collect();
        assert!(
            regions.iter().position(|n| *n == "Aragón")
                < regions.iter().position(|n| *n == "Asturias, Principado de")
        );
    }
}
