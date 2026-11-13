// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! The web pages and their files.
//!
//! The pages take nothing from the request but its language: everything they
//! show they fetch from the API themselves. So each is rendered once per
//! language, at start, and answered from memory. A visitor's language is the
//! one they chose (`?lang=`, kept in a cookie), or else the first of their
//! browser's that there is a catalog for, or else English.
//!
//! The templates are Tera 2 ones, under `templates/`, and the files they use
//! are under `assets/static/`; the strings they translate are in
//! `assets/lang/<code>.json`. All of it is built into the binary.

use super::AppState;
use crate::config::Config;
use anyhow::{Context as _, anyhow};
use axum::Router;
use axum::extract::{Path, Request};
use axum::http::header::{
    ACCEPT_LANGUAGE, CACHE_CONTROL, CONTENT_TYPE, COOKIE, ETAG, HeaderMap, HeaderValue,
    IF_NONE_MATCH, SET_COOKIE, VARY,
};
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use rust_embed::RustEmbed;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tera::{Context, Kwargs, State, Tera};

#[derive(RustEmbed)]
#[folder = "templates/"]
struct Templates;

#[derive(RustEmbed)]
#[folder = "assets/static/"]
struct Static;

#[derive(RustEmbed)]
#[folder = "assets/lang/"]
struct Catalogs;

/// The languages there are pages in. English is the one the templates are
/// written in, and needs no catalog.
pub const LANGUAGES: [&str; 2] = ["en", "es"];

/// Where each page is served, and its template.
const ROUTES: [(&str, &str); 4] = [
    ("/", "map.html"),
    ("/index.html", "map.html"),
    ("/instances", "instances.html"),
    // The old site's address of the instances page.
    ("/static/instances.html", "instances.html"),
];

/// The cookie a chosen language is kept in.
const LANGUAGE_COOKIE: &str = "lang";

/// A language's catalog: English source text to its translation.
pub fn catalog(code: &str) -> HashMap<String, String> {
    Catalogs::get(&format!("{code}.json"))
        .map(|file| serde_json::from_slice(&file.data).expect("the catalogs are valid JSON"))
        .unwrap_or_default()
}

/// A value as JSON that is safe inside a `<script>` element and an HTML
/// attribute.
fn to_json(value: &tera::Value) -> String {
    let json = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
    json.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\'', "\\u0027")
}

/// The templates, in one language.
fn tera(config: &Config, code: &str) -> anyhow::Result<Tera> {
    let translations = Arc::new(catalog(code));
    let mut tera = Tera::new();
    let lookup = Arc::clone(&translations);
    tera.register_filter("tr", move |source: &str, _: Kwargs, _: &State| {
        lookup
            .get(source)
            .cloned()
            .unwrap_or_else(|| source.to_string())
    });
    tera.register_filter("tojson", |value: tera::Value, _: Kwargs, _: &State| {
        tera::Value::safe_string(&to_json(&value))
    });
    let templates = Templates::iter()
        .map(|name| {
            let file = Templates::get(&name).expect("listed");
            let text = String::from_utf8(file.data.into_owned())
                .map_err(|_| anyhow!("template {name} is not UTF-8"))?;
            Ok((name.to_string(), text))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    tera.add_raw_templates(templates)
        .map_err(|e| anyhow!("{e}"))?;

    let globals = tera.global_context();
    globals.insert("ui_language", code);
    globals.insert("ui_translations", translations.as_ref());
    let build = super::current_version();
    globals.insert("build_version", build.version);
    globals.insert(
        "build_revision",
        &Some(build.git_revision_short).filter(|revision| *revision != "unknown"),
    );
    let collector = &config.collector;
    globals.insert("collect_hours", &format_hours(collector.interval_minutes));
    // As text: they go into sentences.
    globals.insert("days_active", &collector.days_active.to_string());
    globals.insert("offline_days", &collector.offline_after_days.to_string());
    globals.insert("delete_days", &collector.delete_after_days.to_string());
    globals.insert("submissions", &config.submissions.enabled);
    globals.insert("per_hour", &config.submissions.per_ip_per_hour);
    Ok(tera)
}

/// An interval in minutes as hours, as the pages say it: "4", or "1.5".
fn format_hours(minutes: u32) -> String {
    if minutes.is_multiple_of(60) {
        (minutes / 60).to_string()
    } else {
        format!("{:.1}", f64::from(minutes) / 60.0)
    }
}

/// A page, ready to answer.
struct Page {
    html: Arc<str>,
    etag: HeaderValue,
}

impl Page {
    fn new(html: String) -> Self {
        let mut hasher = DefaultHasher::new();
        html.hash(&mut hasher);
        Self {
            etag: HeaderValue::from_str(&format!("\"{:016x}\"", hasher.finish())).expect("hex"),
            html: Arc::from(html),
        }
    }
}

/// Every page in every language, by (template, language).
struct Rendered {
    pages: HashMap<(&'static str, &'static str), Page>,
}

fn render(config: &Config) -> anyhow::Result<Rendered> {
    let mut pages = HashMap::new();
    for code in LANGUAGES {
        let tera = tera(config, code)?;
        let templates = ROUTES
            .iter()
            .map(|(_, template)| *template)
            .chain(["error.html"]);
        for template in templates {
            if pages.contains_key(&(template, code)) {
                continue;
            }
            let mut context = Context::new();
            context.insert("page", template.trim_end_matches(".html"));
            let html = tera
                .render(template, &context)
                .map_err(|e| anyhow!("{e}"))
                .with_context(|| format!("rendering {template} in {code}"))?;
            pages.insert((template, code), Page::new(html));
        }
    }
    Ok(Rendered { pages })
}

/// The language to answer in, and whether the request chose it, which is
/// then kept in a cookie.
pub fn language(uri: &Uri, headers: &HeaderMap) -> (&'static str, bool) {
    let known = |code: &str| {
        let code = code.trim().to_ascii_lowercase();
        let primary = code
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_string();
        LANGUAGES.into_iter().find(|known| *known == primary)
    };
    let asked = uri.query().and_then(|query| {
        form_pairs(query)
            .find(|(key, _)| key == "lang")
            .and_then(|(_, value)| known(&value))
    });
    if let Some(code) = asked {
        return (code, true);
    }
    let cookie = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == LANGUAGE_COOKIE)
        .and_then(|(_, value)| known(value));
    if let Some(code) = cookie {
        return (code, false);
    }
    let mut preferred: Vec<(f32, &'static str)> = headers
        .get_all(ACCEPT_LANGUAGE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|entry| {
            let mut parts = entry.split(';');
            let code = known(parts.next()?)?;
            let quality = parts
                .find_map(|param| param.trim().strip_prefix("q="))
                .and_then(|q| q.parse().ok())
                .unwrap_or(1.0);
            (quality > 0.0).then_some((quality, code))
        })
        .collect();
    // Stable, so that equal preferences keep the browser's order.
    preferred.sort_by(|a, b| b.0.total_cmp(&a.0));
    (preferred.first().map_or("en", |(_, code)| code), false)
}

/// The pairs of a query string, decoded.
fn form_pairs(query: &str) -> impl Iterator<Item = (String, String)> + '_ {
    url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned()))
}

fn answer(
    page: &Page,
    status: StatusCode,
    code: &str,
    chosen: bool,
    request: &HeaderMap,
) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(ETAG, page.etag.clone());
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(VARY, HeaderValue::from_static("Accept-Language, Cookie"));
    if chosen {
        let cookie = format!("{LANGUAGE_COOKIE}={code}; Path=/; Max-Age=31536000; SameSite=Lax");
        headers.insert(SET_COOKIE, HeaderValue::from_str(&cookie).expect("ASCII"));
    }
    let fresh = request
        .get(IF_NONE_MATCH)
        .is_some_and(|value| value == page.etag);
    if fresh && !chosen && status == StatusCode::OK {
        return (StatusCode::NOT_MODIFIED, headers).into_response();
    }
    (status, headers, Html(page.html.to_string())).into_response()
}

/// A file under `/static/`. Our own files are revalidated every time, which
/// their ETag makes cheap; the versioned vendor ones are kept for a year.
fn static_file(path: &str, headers: &HeaderMap) -> Option<Response> {
    let file = Static::get(path)?;
    let tag = format!(
        "\"{}\"",
        file.metadata
            .sha256_hash()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let cache = if is_versioned_vendor(path) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let etag = HeaderValue::from_str(&tag).expect("hex");
    let matches = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|candidate| candidate.trim().trim_start_matches("W/") == tag)
        });
    let cache = HeaderValue::from_static(cache);
    if matches {
        return Some(
            (
                StatusCode::NOT_MODIFIED,
                [(ETAG, etag), (CACHE_CONTROL, cache)],
            )
                .into_response(),
        );
    }
    let mime = HeaderValue::from_str(file.metadata.mimetype())
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    Some(
        (
            [(CONTENT_TYPE, mime), (ETAG, etag), (CACHE_CONTROL, cache)],
            file.data.into_owned(),
        )
            .into_response(),
    )
}

/// The bundled libraries live in folders named after their version
/// (`leaflet-1.9.4/`), so what such a path holds never changes.
fn is_versioned_vendor(path: &str) -> bool {
    path.strip_prefix("vendor/")
        .and_then(|rest| rest.split_once('/'))
        .and_then(|(folder, _)| folder.rsplit_once('-'))
        .is_some_and(|(name, version)| {
            !name.is_empty()
                && !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// The pages, rendered.
#[derive(Clone)]
pub struct Pages {
    rendered: Arc<Rendered>,
}

impl Pages {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            rendered: Arc::new(render(config)?),
        })
    }

    /// The routes of the pages and their files.
    pub fn router(&self) -> Router<AppState> {
        let mut router = Router::new();
        for (route, template) in ROUTES {
            let rendered = Arc::clone(&self.rendered);
            router = router.route(
                route,
                get(move |request: Request| async move {
                    let (code, chosen) = language(request.uri(), request.headers());
                    answer(
                        &rendered.pages[&(template, code)],
                        StatusCode::OK,
                        code,
                        chosen,
                        request.headers(),
                    )
                }),
            );
        }
        // The old site's map page.
        router = router.route(
            "/static/index.html",
            get(|| async { axum::response::Redirect::permanent("/") }),
        );
        let pages = self.clone();
        router.route(
            "/static/{*path}",
            get(
                move |Path(path): Path<String>, request: Request| async move {
                    static_file(&path, request.headers())
                        .unwrap_or_else(|| pages.not_found(&request))
                },
            ),
        )
    }

    /// The error page, for a page that is not there.
    pub fn not_found(&self, request: &Request) -> Response {
        let (code, chosen) = language(request.uri(), request.headers());
        answer(
            &self.rendered.pages[&("error.html", code)],
            StatusCode::NOT_FOUND,
            code,
            chosen,
            request.headers(),
        )
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
    fn the_language_is_the_one_chosen_then_the_browser_s() {
        let uri = |s: &str| s.parse::<Uri>().unwrap();
        assert_eq!(language(&uri("/"), &HeaderMap::new()), ("en", false));
        assert_eq!(language(&uri("/?lang=es"), &HeaderMap::new()), ("es", true));
        assert_eq!(
            language(&uri("/?lang=ru"), &HeaderMap::new()),
            ("en", false)
        );
        assert_eq!(
            language(
                &uri("/"),
                &headers(&[("accept-language", "gl-ES,es;q=0.9,en;q=0.8")])
            ),
            ("es", false)
        );
        assert_eq!(
            language(
                &uri("/"),
                &headers(&[("accept-language", "en-GB,es;q=0.5")])
            ),
            ("en", false)
        );
        assert_eq!(
            language(&uri("/"), &headers(&[("accept-language", "es;q=0,en")])),
            ("en", false)
        );
        // A choice kept wins over the browser.
        assert_eq!(
            language(
                &uri("/"),
                &headers(&[("accept-language", "es"), ("cookie", "a=b; lang=en")])
            ),
            ("en", false)
        );
    }

    #[test]
    fn versioned_vendor_folders_are_recognised() {
        assert!(is_versioned_vendor("vendor/leaflet-1.9.4/dist/leaflet.js"));
        assert!(is_versioned_vendor(
            "vendor/leaflet.markercluster-1.5.3/dist/MarkerCluster.css"
        ));
        assert!(!is_versioned_vendor("vendor/manifest.json"));
        assert!(!is_versioned_vendor("theme.js"));
    }

    #[test]
    fn hours_are_said_plainly() {
        assert_eq!(format_hours(240), "4");
        assert_eq!(format_hours(90), "1.5");
    }

    #[test]
    fn json_in_a_page_cannot_close_its_script() {
        let value = tera::Value::from("</script><b>'&");
        assert!(!to_json(&value).contains('<'));
    }

    /// The strings a template or a script asks to translate: `"…" | tr` in
    /// templates, and `MeshI18n.t("…")` or `MeshI18n.h("…")` in scripts.
    fn translated_strings(text: &str) -> Vec<String> {
        let mut found: Vec<String> = text
            .match_indices("\" | tr")
            .filter_map(|(end, _)| {
                let start = text[..end].rfind('"')? + 1;
                Some(text[start..end].to_string())
            })
            .collect();
        for call in ["MeshI18n.t(\"", "MeshI18n.h(\""] {
            for (start, _) in text.match_indices(call) {
                let rest = &text[start + call.len()..];
                if let Some(end) = rest.find('"') {
                    found.push(rest[..end].to_string());
                }
            }
        }
        found
    }

    /// Every string the pages translate is in every catalog, so that no page
    /// shows a string untranslated, and no catalog keeps strings nothing asks
    /// for.
    #[test]
    fn every_translated_string_is_in_every_catalog() {
        let mut needed = std::collections::BTreeSet::new();
        for name in Templates::iter() {
            let file = Templates::get(&name).unwrap();
            needed.extend(translated_strings(std::str::from_utf8(&file.data).unwrap()));
        }
        for name in
            Static::iter().filter(|name| name.ends_with(".js") && !name.starts_with("vendor/"))
        {
            let file = Static::get(&name).unwrap();
            needed.extend(translated_strings(std::str::from_utf8(&file.data).unwrap()));
        }
        assert!(needed.len() > 50, "{needed:?}");
        for code in LANGUAGES.into_iter().filter(|code| *code != "en") {
            let catalog = catalog(code);
            let missing: Vec<_> = needed
                .iter()
                .filter(|s| !catalog.contains_key(*s))
                .collect();
            assert!(
                missing.is_empty(),
                "assets/lang/{code}.json lacks {missing:#?}"
            );
            let unused: Vec<_> = catalog.keys().filter(|s| !needed.contains(*s)).collect();
            assert!(
                unused.is_empty(),
                "assets/lang/{code}.json has unused {unused:#?}"
            );
        }
    }

    #[test]
    fn every_page_renders_in_every_language() {
        let rendered = render(&Config::default()).unwrap();
        for code in LANGUAGES {
            for template in ["map.html", "instances.html", "error.html"] {
                let html = &rendered.pages[&(template, code)].html;
                assert!(
                    html.contains(&format!("<html lang=\"{code}\"")),
                    "{template} {code}"
                );
            }
        }
        let spanish = &rendered.pages[&("instances.html", "es")].html;
        assert!(spanish.contains("Instancias"));
    }
}
