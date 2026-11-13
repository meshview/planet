// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Reaching the instances: their URLs, the HTTP client, and keeping it out of
//! the network this server runs in.
//!
//! Anyone can submit a URL, and this server will then request it every few
//! hours. Unless told otherwise it only ever connects to public addresses:
//! a URL naming a private one is refused, a name that resolves only to
//! private ones fails to resolve, and so does a redirect to one. Checking the
//! addresses at connection time, in the resolver, rather than once when the URL
//! is submitted, is what keeps a name that changes what it points to from
//! getting round it.

use crate::config::Collector;
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use url::{Host, Url};

/// How many redirects a request follows.
const MAX_REDIRECTS: usize = 5;

/// What the instances see as the client.
const USER_AGENT: &str = concat!(
    "meshview-planet/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/meshview/planet)"
);

/// Why a URL was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlError {
    Invalid,
    Scheme,
    Credentials,
    Private,
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "not a valid URL",
            Self::Scheme => "only http and https URLs are accepted",
            Self::Credentials => "a URL with a user or a password is not accepted",
            Self::Private => "the URL points to a private address",
        })
    }
}

impl std::error::Error for UrlError {}

/// Brings an instance's URL to the one form it is stored in, so that the same
/// instance is not added twice in two spellings: scheme and host in lower case
/// (which the parser does), no default port, no query or fragment, and no
/// trailing slash. The path stays, as several instances live under one host.
/// A URL without a scheme is taken as https.
pub fn normalise_url(input: &str, allow_private: bool) -> Result<String, UrlError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(UrlError::Invalid);
    }
    let with_scheme = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let mut url = Url::parse(&with_scheme).map_err(|_| UrlError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlError::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::Credentials);
    }
    let Some(host) = url.host() else {
        return Err(UrlError::Invalid);
    };
    if !allow_private && !host_may_be_public(&host) {
        return Err(UrlError::Private);
    }
    url.set_query(None);
    url.set_fragment(None);

    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    let mut text = url.to_string();
    while text.ends_with('/') {
        text.pop();
    }
    Ok(text)
}

/// Whether a host may be a public one: any name (its addresses are checked when
/// it is resolved), or an address that is public.
fn host_may_be_public(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(name) => {
            let name = name.trim_end_matches('.');
            name != "localhost" && !name.ends_with(".localhost")
        }
        Host::Ipv4(ip) => is_public(IpAddr::V4(*ip)),
        Host::Ipv6(ip) => is_public(IpAddr::V6(*ip)),
    }
}

/// Whether an address is on the public internet: not loopback, private,
/// link-local, shared (carrier-grade NAT), reserved, documentation, multicast
/// or unspecified.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(ip),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        // 100.64.0.0/10, shared address space.
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24, protocol assignments.
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15, benchmarking.
        || (a == 198 && (b == 18 || b == 19))
        // 240.0.0.0/4, reserved.
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // fc00::/7, unique local.
        || (first & 0xfe00) == 0xfc00
        // fe80::/10, link-local.
        || (first & 0xffc0) == 0xfe80
        // 2001:db8::/32, documentation.
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        // 64:ff9b:1::/48, local use NAT64.
        || (first == 0x0064 && ip.segments()[1] == 0xff9b && ip.segments()[2] == 1))
}

/// Resolves names the system's way, keeping only public addresses.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let found: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| is_public(addr.ip()))
                .collect();
            if found.is_empty() {
                return Err(format!("{host} resolves to no public address").into());
            }
            Ok(Box::new(found.into_iter()) as Addrs)
        })
    }
}

/// TLS through rustls with `ring`, trusting the roots compiled in: no
/// ca-certificates is needed where it runs.
pub fn tls() -> Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("setting up TLS")?
    .with_root_certificates(roots)
    .with_no_client_auth())
}

/// The client the instances are read with.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    max_bytes: usize,
    allow_private: bool,
}

impl Client {
    pub fn new(settings: &Collector) -> Result<Self> {
        let allow_private = settings.allow_private_addresses;
        let policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            let url = attempt.url();
            let allowed = matches!(url.scheme(), "http" | "https")
                && (allow_private || url.host().is_some_and(|host| host_may_be_public(&host)));
            if allowed {
                attempt.follow()
            } else {
                attempt.error("redirected to a URL that is not allowed")
            }
        });

        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(settings.timeout())
            .redirect(policy)
            .tls_backend_preconfigured(tls()?)
            .no_proxy();
        if !allow_private {
            builder = builder.dns_resolver(Arc::new(PublicOnly));
        }
        Ok(Self {
            http: builder.build().context("building the HTTP client")?,
            max_bytes: settings.max_response_bytes(),
            allow_private,
        })
    }

    /// GETs a URL and answers its body, refusing an answer that is not a
    /// success or is larger than the limit.
    pub async fn get(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        // The resolver never sees an address written in the URL itself.
        if !self.allow_private {
            let public = Url::parse(url)
                .ok()
                .is_some_and(|url| url.host().is_some_and(|host| host_may_be_public(&host)));
            if !public {
                return Err(FetchError::Connection(format!(
                    "{url} points to a private address"
                )));
            }
        }
        let response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(FetchError::from_reqwest)?;
        let status = response.status();
        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_bytes as u64)
        {
            return Err(FetchError::TooLarge(self.max_bytes));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(FetchError::from_reqwest)?;
            if body.len() + chunk.len() > self.max_bytes {
                return Err(FetchError::TooLarge(self.max_bytes));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// Why a request got nothing.
#[derive(Debug)]
pub enum FetchError {
    /// It answered, with this status.
    Status(u16),
    /// Its answer was larger than this many bytes.
    TooLarge(usize),
    /// It did not answer in time.
    Timeout,
    /// It could not be reached, or the connection failed.
    Connection(String),
}

impl FetchError {
    fn from_reqwest(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else {
            Self::Connection(error_chain(&error))
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(status) => write!(f, "answered HTTP {status}"),
            Self::TooLarge(max) => write!(f, "answer larger than {} MB", max / 1024 / 1024),
            Self::Timeout => f.write_str("did not answer in time"),
            Self::Connection(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for FetchError {}

/// An error and its causes in one line: reqwest's own message alone is often
/// just "error sending request".
fn error_chain(error: &dyn std::error::Error) -> String {
    // reqwest's own message only repeats the URL, which is the instance's.
    let mut text = error.to_string();
    if text.starts_with("error sending request") && error.source().is_some() {
        text.clear();
    }
    let mut source = error.source();
    while let Some(cause) = source {
        let message = cause.to_string();
        if !text.contains(&message) {
            if !text.is_empty() {
                text.push_str(": ");
            }
            text.push_str(&message);
        }
        source = cause.source();
    }
    text
}

/// Checks a URL the way a submission is, for the import, which takes them from
/// another server.
pub fn check_url(input: &str, allow_private: bool) -> Result<String> {
    match normalise_url(input, allow_private) {
        Ok(url) => Ok(url),
        Err(e) => bail!("{input}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_brought_to_one_form() {
        let n = |url| normalise_url(url, false);
        assert_eq!(
            n("https://Meshview.Example.org/").unwrap(),
            "https://meshview.example.org"
        );
        assert_eq!(
            n("meshview.example.org").unwrap(),
            "https://meshview.example.org"
        );
        assert_eq!(
            n("https://mesh.example.org/anz/?x=1#map").unwrap(),
            "https://mesh.example.org/anz"
        );
        assert_eq!(n("http://example.org:80/").unwrap(), "http://example.org");
        assert_eq!(
            n("https://example.org:8443").unwrap(),
            "https://example.org:8443"
        );
        assert_eq!(
            n("  https://example.org//  ").unwrap(),
            "https://example.org"
        );
    }

    #[test]
    fn bad_urls_are_refused() {
        let n = |url| normalise_url(url, false);
        assert_eq!(n(""), Err(UrlError::Invalid));
        assert_eq!(n("ftp://example.org"), Err(UrlError::Scheme));
        assert_eq!(n("javascript:alert(1)"), Err(UrlError::Invalid));
        assert_eq!(n("https://user:pw@example.org"), Err(UrlError::Credentials));
        assert_eq!(n("https://"), Err(UrlError::Invalid));
    }

    #[test]
    fn private_hosts_are_refused_unless_allowed() {
        for url in [
            "http://127.0.0.1:8080",
            "http://localhost",
            "http://foo.localhost",
            "http://10.1.2.3",
            "http://192.168.1.1",
            "http://172.16.0.1",
            "http://169.254.169.254",
            "http://100.64.0.1",
            "http://0.0.0.0",
            "http://[::1]",
            "http://[fd00::1]",
            "http://[fe80::1]",
            "http://[::ffff:127.0.0.1]",
        ] {
            assert_eq!(normalise_url(url, false), Err(UrlError::Private), "{url}");
            assert!(normalise_url(url, true).is_ok(), "{url}");
        }
        assert!(normalise_url("http://8.8.8.8", false).is_ok());
        assert!(normalise_url("http://[2a00:1450::1]", false).is_ok());
    }
}
