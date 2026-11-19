// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Telling the administrator what happens to the list: an instance added from
//! the site and, if they want, one going offline, coming back or being
//! deleted.
//!
//! Each channel configured gets every event asked for: a generic webhook, as
//! JSON a program can read; ntfy; Discord; Slack; a Matrix room; and a
//! Telegram chat. Notices
//! are sent in the background, so a channel that is down never holds up a
//! submission or a collection: what fails is logged, and that is all.
//!
//! The channels are the administrator's own, so unlike the instances they may
//! be on a private network, as a home ntfy server is.

use crate::areas::{Areas, Language};
use crate::config::{Event, Notifications};
use crate::net::tls;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// How long a channel gets to take a notice.
const TIMEOUT: Duration = Duration::from_secs(15);

/// How long the notices still on their way get when the process stops.
const FLUSH_GRACE: Duration = Duration::from_secs(10);

/// Something that happened to an instance.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub event: Event,
    pub url: String,
    pub country: String,
    pub region: Option<String>,
    /// What it runs, when that is known.
    pub version: Option<String>,
    /// Which Meshview, when it says: `rust` for Meshview Rust.
    pub flavor: Option<String>,
    /// Its nodes, when it just answered.
    pub node_count: Option<usize>,
    /// Why it is not answering, for one going offline or deleted.
    pub last_error: Option<String>,
    /// The days that made it go offline, or be deleted.
    pub days: Option<u32>,
    /// Who added it, for one added from the site.
    pub from_ip: Option<IpAddr>,
    /// When, in unix seconds.
    pub at: i64,
}

impl Notice {
    fn new(event: Event, url: &str, country: &str, region: Option<&str>, at: i64) -> Self {
        Self {
            event,
            url: url.to_string(),
            country: country.to_string(),
            region: region.map(str::to_string),
            version: None,
            flavor: None,
            node_count: None,
            last_error: None,
            days: None,
            from_ip: None,
            at,
        }
    }

    /// An instance added from the site.
    pub fn added(
        url: &str,
        country: &str,
        region: Option<&str>,
        version: &str,
        nodes: usize,
        from_ip: IpAddr,
        at: i64,
    ) -> Self {
        Self {
            version: Some(version.to_string()),
            node_count: Some(nodes),
            from_ip: Some(from_ip),
            ..Self::new(Event::InstanceAdded, url, country, region, at)
        }
    }

    /// An instance gone offline.
    pub fn offline(
        url: &str,
        country: &str,
        region: Option<&str>,
        last_error: Option<&str>,
        days: u32,
        at: i64,
    ) -> Self {
        Self {
            last_error: last_error.map(str::to_string),
            days: Some(days),
            ..Self::new(Event::InstanceOffline, url, country, region, at)
        }
    }

    /// An offline instance answering again.
    pub fn online(
        url: &str,
        country: &str,
        region: Option<&str>,
        version: &str,
        nodes: usize,
        at: i64,
    ) -> Self {
        Self {
            version: Some(version.to_string()),
            node_count: Some(nodes),
            ..Self::new(Event::InstanceOnline, url, country, region, at)
        }
    }

    /// An instance deleted after being offline.
    pub fn deleted(
        url: &str,
        country: &str,
        region: Option<&str>,
        last_error: Option<&str>,
        days: u32,
        at: i64,
    ) -> Self {
        Self {
            last_error: last_error.map(str::to_string),
            days: Some(days),
            ..Self::new(Event::InstanceDeleted, url, country, region, at)
        }
    }

    /// The same notice, saying which Meshview the instance runs, as its
    /// /version does.
    pub fn with_flavor(self, flavor: Option<&str>) -> Self {
        Self {
            flavor: flavor.map(str::to_string),
            ..self
        }
    }

    /// A test notice, to check the channels. Its words say it is one; for a
    /// program reading the webhook, it carries every field a real notice
    /// does, about an example instance (on a domain and from an address kept
    /// for examples), so that one can be built against it.
    pub fn test(at: i64) -> Self {
        Self {
            version: Some("1.0.0".to_string()),
            flavor: Some("rust".to_string()),
            node_count: Some(0),
            from_ip: Some(IpAddr::from([192, 0, 2, 1])),
            ..Self::new(
                Event::Test,
                "https://meshview.example.org",
                "ES",
                Some("ES-GA"),
                at,
            )
        }
    }

    fn area(&self, language: Language) -> String {
        Areas::get().label(&self.country, self.region.as_deref(), language)
    }

    /// One line saying what happened, in English: notices are for the
    /// administrator, not for the site's visitors.
    pub fn title(&self) -> String {
        let area = self.area(Language::English);
        match self.event {
            Event::InstanceAdded => format!("New instance: {area}"),
            Event::InstanceOffline => format!("Instance offline: {area}"),
            Event::InstanceOnline => format!("Instance back online: {area}"),
            Event::InstanceDeleted => format!("Instance deleted: {area}"),
            Event::Test => "Test notice from Meshview Rust Planet".to_string(),
        }
    }

    /// The details, starting with the instance's address but for a test,
    /// whose instance is only an example.
    pub fn message(&self) -> String {
        let days = self.days.unwrap_or_default();
        let nodes = self.node_count.unwrap_or_default();
        let version = self.version.as_deref().unwrap_or("unknown");
        let meshview = match self.flavor.as_deref() {
            Some("rust") => "Meshview Rust",
            _ => "Meshview",
        };
        let reason = self
            .last_error
            .as_deref()
            .map(|error| format!(" The latest error: {error}."))
            .unwrap_or_default();
        let detail = match self.event {
            Event::InstanceAdded => {
                let from = self
                    .from_ip
                    .map(|ip| format!(", from {ip}"))
                    .unwrap_or_default();
                format!("{meshview} {version}, {nodes} nodes. Added from the site{from}.")
            }
            Event::InstanceOffline => format!(
                "No answer in {days} days: its nodes are off the map until it answers again.{reason}"
            ),
            Event::InstanceOnline => {
                format!("Answering again: {meshview} {version}, {nodes} nodes.")
            }
            Event::InstanceDeleted => {
                format!("Offline for {days} days: deleted, with its history.{reason}")
            }
            Event::Test => "If you read this, notices reach this channel.".to_string(),
        };
        if self.event == Event::Test {
            return detail;
        }
        format!("{}\n{detail}", self.url)
    }

    /// The notice as the generic webhook sends it.
    pub fn json(&self) -> Value {
        json!({
            "event": self.event.as_str(),
            "title": self.title(),
            "message": self.message(),
            "at": self.at,
            "instance": {
                "url": self.url,
                "country": self.country,
                "region": self.region,
                "area": {
                    "en": self.area(Language::English),
                    "es": self.area(Language::Spanish),
                },
                "version": self.version,
                "flavor": self.flavor,
                "node_count": self.node_count,
                "last_error": self.last_error,
            },
            "days": self.days,
            "from_ip": self.from_ip.map(|ip| ip.to_string()),
        })
    }

    fn ntfy_tags(&self) -> &'static [&'static str] {
        match self.event {
            Event::InstanceAdded => &["new"],
            Event::InstanceOffline => &["warning"],
            Event::InstanceOnline => &["white_check_mark"],
            Event::InstanceDeleted => &["wastebasket"],
            Event::Test => &["test_tube"],
        }
    }
}

/// A channel notices go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Webhook,
    Ntfy,
    Discord,
    Slack,
    Matrix,
    Telegram,
}

impl Channel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Ntfy => "ntfy",
            Self::Discord => "Discord",
            Self::Slack => "Slack",
            Self::Matrix => "Matrix",
            Self::Telegram => "Telegram",
        }
    }
}

struct Inner {
    http: reqwest::Client,
    config: Notifications,
    channels: Vec<Channel>,
    /// Notices on their way, for waiting on them when the process stops.
    pending: Mutex<Vec<JoinHandle<()>>>,
    /// For Matrix, which wants every message sent with an id of its own.
    sent: AtomicU64,
}

/// Sends notices, or does nothing when no channel is configured.
#[derive(Clone, Default)]
pub struct Notifier {
    inner: Option<Arc<Inner>>,
}

impl Notifier {
    /// The channels configured, if any event is asked for.
    pub fn new(config: &Notifications) -> Result<Self> {
        if config.events.is_empty() {
            return Ok(Self::default());
        }
        Self::build(config)
    }

    /// The channels configured, whatever the events: for a test notice,
    /// which is worth sending before any event is asked for.
    pub fn testing(config: &Notifications) -> Result<Self> {
        Self::build(config)
    }

    fn build(config: &Notifications) -> Result<Self> {
        let set = |value: &str| !value.trim().is_empty();
        let mut channels = Vec::new();
        if set(&config.webhook_url) {
            channels.push(Channel::Webhook);
        }
        if set(&config.ntfy_url) {
            channels.push(Channel::Ntfy);
        }
        if set(&config.discord_url) {
            channels.push(Channel::Discord);
        }
        if set(&config.slack_url) {
            channels.push(Channel::Slack);
        }
        if set(&config.matrix_homeserver) {
            channels.push(Channel::Matrix);
        }
        if set(&config.telegram_token) {
            channels.push(Channel::Telegram);
        }
        if channels.is_empty() {
            return Ok(Self::default());
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("meshview-planet/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .tls_backend_preconfigured(tls()?)
            .build()
            .context("building the notifications' HTTP client")?;
        Ok(Self {
            inner: Some(Arc::new(Inner {
                http,
                config: config.clone(),
                channels,
                pending: Mutex::new(Vec::new()),
                sent: AtomicU64::new(0),
            })),
        })
    }

    /// The channels notices go to, for the log at start.
    pub fn channels(&self) -> Vec<Channel> {
        self.inner
            .as_ref()
            .map(|inner| inner.channels.clone())
            .unwrap_or_default()
    }

    /// Sends a notice to every channel, in the background, if its event is
    /// one asked for.
    pub fn send(&self, notice: Notice) {
        let Some(inner) = &self.inner else {
            return;
        };
        if !inner.config.events.contains(&notice.event) {
            return;
        }
        let notifier = self.clone();
        let task = tokio::spawn(async move {
            for (channel, result) in notifier.deliver(&notice).await {
                match result {
                    Ok(()) => debug!("{} told about {}", channel.name(), notice.url),
                    Err(e) => warn!(
                        "could not tell {} about {}: {e:#}",
                        channel.name(),
                        notice.url
                    ),
                }
            }
        });
        let mut pending = inner.pending.lock().expect("not poisoned");
        pending.retain(|task| !task.is_finished());
        pending.push(task);
    }

    /// Waits a while for the notices still on their way: for a process about
    /// to stop.
    pub async fn flush(&self) {
        let Some(inner) = &self.inner else {
            return;
        };
        let pending: Vec<_> = inner
            .pending
            .lock()
            .expect("not poisoned")
            .drain(..)
            .collect();
        let all = futures_util::future::join_all(pending);
        if tokio::time::timeout(FLUSH_GRACE, all).await.is_err() {
            warn!("some notices were still on their way after {FLUSH_GRACE:?}; left unsent");
        }
    }

    /// Sends a notice to every channel now, whatever its event, and answers
    /// how each went.
    pub async fn deliver(&self, notice: &Notice) -> Vec<(Channel, Result<()>)> {
        let Some(inner) = &self.inner else {
            return Vec::new();
        };
        let mut results = Vec::new();
        for &channel in &inner.channels {
            let result = match channel {
                Channel::Webhook => self.webhook(inner, notice).await,
                Channel::Ntfy => self.ntfy(inner, notice).await,
                Channel::Discord => self.discord(inner, notice).await,
                Channel::Slack => self.slack(inner, notice).await,
                Channel::Matrix => self.matrix(inner, notice).await,
                Channel::Telegram => self.telegram(inner, notice).await,
            };
            results.push((channel, result));
        }
        results
    }

    async fn post(&self, inner: &Inner, url: &str, body: &Value, token: &str) -> Result<()> {
        let mut request = inner.http.post(url).json(body);
        if !token.trim().is_empty() {
            request = request.bearer_auth(token.trim());
        }
        check(request.send().await.map_err(without_url)?).await
    }

    async fn webhook(&self, inner: &Inner, notice: &Notice) -> Result<()> {
        self.post(inner, inner.config.webhook_url.trim(), &notice.json(), "")
            .await
    }

    /// ntfy, through its JSON publishing, which takes any text: the topic's
    /// URL is split into the server, where the JSON goes, and the topic.
    async fn ntfy(&self, inner: &Inner, notice: &Notice) -> Result<()> {
        let mut url = url::Url::parse(inner.config.ntfy_url.trim()).context("ntfy_url")?;
        let topic = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|topic| !topic.is_empty())
            .map(str::to_string);
        let Some(topic) = topic else {
            bail!("ntfy_url has no topic, as in https://ntfy.sh/my-topic");
        };
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("ntfy_url is not a server's URL"))?
            .pop();
        let mut body = json!({
            "topic": topic,
            "title": notice.title(),
            "message": notice.message(),
            "tags": notice.ntfy_tags(),
        });
        // A click opens the instance; a test's is only an example.
        if notice.event != Event::Test {
            body["click"] = json!(notice.url);
        }
        self.post(inner, url.as_str(), &body, &inner.config.ntfy_token)
            .await
    }

    async fn discord(&self, inner: &Inner, notice: &Notice) -> Result<()> {
        let body = json!({
            "content": format!("**{}**\n{}", notice.title(), notice.message()),
            // Nothing in a notice is meant to ping anybody.
            "allowed_mentions": {"parse": []},
        });
        self.post(inner, inner.config.discord_url.trim(), &body, "")
            .await
    }

    async fn slack(&self, inner: &Inner, notice: &Notice) -> Result<()> {
        let escape = |text: String| {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        let body = json!({
            "text": format!("*{}*\n{}", escape(notice.title()), escape(notice.message())),
        });
        self.post(inner, inner.config.slack_url.trim(), &body, "")
            .await
    }

    async fn matrix(&self, inner: &Inner, notice: &Notice) -> Result<()> {
        let config = &inner.config;
        let room: String =
            url::form_urlencoded::byte_serialize(config.matrix_room.trim().as_bytes()).collect();
        let id = format!(
            "meshview-planet-{}-{}",
            notice.at,
            inner.sent.fetch_add(1, Ordering::Relaxed)
        );
        let url = format!(
            "{}/_matrix/client/v3/rooms/{room}/send/m.room.message/{id}",
            config.matrix_homeserver.trim().trim_end_matches('/')
        );
        let body = json!({
            "msgtype": "m.text",
            "body": format!("{}\n{}", notice.title(), notice.message()),
        });
        let request = inner
            .http
            .put(url)
            .bearer_auth(config.matrix_token.trim())
            .json(&body);
        check(request.send().await.map_err(without_url)?).await
    }

    /// Telegram's Bot API, in plain text: nothing a submitter wrote is read
    /// as Markdown or HTML.
    async fn telegram(&self, inner: &Inner, notice: &Notice) -> Result<()> {
        let config = &inner.config;
        let url = format!(
            "{}/bot{}/sendMessage",
            config.telegram_api(),
            config.telegram_token.trim()
        );
        let body = json!({
            "chat_id": config.telegram_chat_id.trim(),
            "text": format!("{}\n{}", notice.title(), notice.message()),
            "link_preview_options": {"is_disabled": true},
        });
        self.post(inner, &url, &body, "").await
    }
}

/// A request's error without its URL, which for Telegram holds the bot's
/// token, and for Discord and Slack their webhook's secret: errors go to the
/// log.
fn without_url(error: reqwest::Error) -> reqwest::Error {
    error.without_url()
}

/// A failure when the channel did not take the notice, with what it said.
async fn check(response: reqwest::Response) -> Result<()> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let text = response.text().await.unwrap_or_default();
    let text: String = text.chars().take(200).collect();
    bail!("answered {status}: {}", text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn added() -> Notice {
        Notice::added(
            "https://meshview.example.org",
            "ES",
            Some("ES-GA"),
            "5.0.0",
            812,
            "203.0.113.7".parse().unwrap(),
            1_794_571_200,
        )
    }

    #[test]
    fn notices_say_what_happened() {
        let notice = added();
        assert_eq!(notice.title(), "New instance: Galicia, Spain");
        assert_eq!(
            notice.message(),
            "https://meshview.example.org\nMeshview 5.0.0, 812 nodes. Added from the site, from 203.0.113.7."
        );
        let offline = Notice::offline(
            "https://x.example.org",
            "US",
            None,
            Some("/version: answered HTTP 502"),
            3,
            0,
        );
        assert_eq!(offline.title(), "Instance offline: United States");
        assert!(
            offline
                .message()
                .ends_with("The latest error: /version: answered HTTP 502.")
        );
        let deleted = Notice::deleted("https://x.example.org", "US", None, None, 30, 0);
        assert!(deleted.message().contains("Offline for 30 days"));
    }

    #[test]
    fn the_webhook_gets_everything_a_program_needs() {
        let json = added().json();
        assert_eq!(json["event"], "instance_added");
        assert_eq!(json["instance"]["url"], "https://meshview.example.org");
        assert_eq!(json["instance"]["region"], "ES-GA");
        assert_eq!(json["instance"]["area"]["es"], "Galicia, España");
        assert_eq!(json["instance"]["node_count"], 812);
        assert_eq!(json["instance"]["flavor"], Value::Null);
        let rust = Notice::online("https://x.example.org", "ES", None, "1.0.0", 3, 0)
            .with_flavor(Some("rust"));
        assert_eq!(rust.json()["instance"]["flavor"], "rust");
        assert!(
            rust.message()
                .ends_with("Answering again: Meshview Rust 1.0.0, 3 nodes.")
        );
        assert_eq!(json["from_ip"], "203.0.113.7");
    }

    #[test]
    fn nothing_configured_is_nothing_to_do() {
        let notifier = Notifier::new(&Notifications::default()).unwrap();
        assert!(notifier.channels().is_empty());
        assert!(
            Notifier::testing(&Notifications::default())
                .unwrap()
                .channels()
                .is_empty()
        );
    }

    #[test]
    fn a_test_reaches_the_channels_with_no_event_asked_for() {
        let config = Notifications {
            events: Vec::new(),
            ntfy_url: "https://ntfy.example.org/planet".into(),
            ..Notifications::default()
        };
        assert!(Notifier::new(&config).unwrap().channels().is_empty());
        assert_eq!(
            Notifier::testing(&config).unwrap().channels(),
            [Channel::Ntfy]
        );
    }

    #[test]
    fn a_test_notice_carries_what_a_real_one_does() {
        let notice = Notice::test(1_794_571_200);
        assert_eq!(notice.title(), "Test notice from Meshview Rust Planet");
        assert!(!notice.message().contains("http"));
        // The webhook gets every key a real notice has, with its type.
        let test = notice.json();
        let real = added().json();
        let keys = |value: &Value| {
            let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        assert_eq!(keys(&test), keys(&real));
        assert_eq!(keys(&test["instance"]), keys(&real["instance"]));
        assert_eq!(test["event"], "test");
        assert_eq!(test["instance"]["area"]["en"], "Galicia, Spain");
        assert_eq!(test["from_ip"], "192.0.2.1");
        // Nobody can ask for it in the configuration.
        assert!(crate::config::Config::parse("[notifications]\nevents = [\"test\"]").is_err());
    }
}
