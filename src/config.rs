// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Configuration: a TOML file, with environment variables on top.
//!
//! Every key has a default, so an empty file, or none, is a valid
//! configuration. A key or a section this program does not know is an error,
//! so a typo is found rather than quietly ignored.
//!
//! Any value can be overridden from the environment as
//! `MESHVIEW_PLANET_<SECTION>__<KEY>`, read as the type the key has:
//! `MESHVIEW_PLANET_SERVER__PORT=8080` is a number and
//! `MESHVIEW_PLANET_COLLECTOR__ALLOW_PRIVATE_ADDRESSES=true` a boolean.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml::{Table, Value};

/// The prefix every configuration variable in the environment carries.
const ENV_PREFIX: &str = "MESHVIEW_PLANET_";

/// Between the section and the key in an environment variable's name. Double,
/// because both may contain single ones.
const ENV_SEPARATOR: &str = "__";

/// Where the configuration is looked for when none is named, in order.
pub const SEARCHED: [&str; 2] = ["config.toml", "/etc/meshview-planet/config.toml"];

/// The whole configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub site: Site,
    pub database: Database,
    pub collector: Collector,
    pub submissions: Submissions,
    pub notifications: Notifications,
    /// The file this came from, if any.
    #[serde(skip)]
    source: Option<PathBuf>,
    /// The `MESHVIEW_PLANET_` variables laid over it, by name.
    #[serde(skip)]
    environment: Vec<String>,
}

/// `[server]`: where the web server listens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    /// The address to listen on. `*` means every interface.
    pub bind: String,
    pub port: u16,
    /// Whether to take the client's address from `X-Forwarded-For`, for the
    /// submission rate limit. Only behind a reverse proxy that sets it: anyone
    /// can send the header to a server that answers directly.
    pub behind_proxy: bool,
    /// The address the site is reached at, as `planet.example.org` (HTTPS
    /// unless it names its scheme), for link previews, which need absolute
    /// addresses. Empty: taken from each request.
    pub domain: String,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind: "*".into(),
            port: 8080,
            behind_proxy: false,
            domain: String::new(),
        }
    }
}

impl Server {
    /// Where the site is, as `https://planet.example.org`, if configured.
    pub fn origin(&self) -> Option<String> {
        let domain = self.domain.trim().trim_end_matches('/');
        if domain.is_empty() {
            None
        } else if domain.starts_with("https://") || domain.starts_with("http://") {
            Some(domain.to_string())
        } else {
            Some(format!("https://{domain}"))
        }
    }
}

/// `[database]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Database {
    /// The SQLite file. Created, with its schema, if it is not there.
    pub path: PathBuf,
}

impl Default for Database {
    fn default() -> Self {
        Self {
            path: "planet.db".into(),
        }
    }
}

/// `[collector]`: reading the instances, and forgetting those that stop
/// answering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Collector {
    /// Minutes between collections.
    pub interval_minutes: u32,
    /// The nodes asked of each instance: those seen in this many days.
    pub days_active: u32,
    /// How many instances are read at the same time.
    pub concurrency: u32,
    /// Seconds an instance gets to answer each request, body included.
    pub timeout_seconds: u32,
    /// The largest answer read from an instance, in megabytes.
    pub max_response_mb: u32,
    /// Days without a good collection after which an instance is offline: at
    /// the end of the list, and its nodes off the map.
    pub offline_after_days: u32,
    /// Days offline after which an instance is deleted, with its nodes and its
    /// history.
    pub delete_after_days: u32,
    /// Whether instances on private, loopback or link-local addresses may be
    /// read. Off, a submission cannot make this server reach into the network
    /// it runs in. Only for testing.
    pub allow_private_addresses: bool,
}

impl Default for Collector {
    fn default() -> Self {
        Self {
            interval_minutes: 240,
            days_active: 14,
            concurrency: 8,
            timeout_seconds: 60,
            max_response_mb: 64,
            offline_after_days: 3,
            delete_after_days: 30,
            allow_private_addresses: false,
        }
    }
}

/// `[site]`: how the pages behave.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Site {
    /// The map's kiosk mode (`/?kiosk`) without its full screen button, so
    /// that nothing on the screen leaves it: for a kiosk proper.
    pub map_kiosk_lock: bool,
}

/// `[submissions]`: instances added through the web.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Submissions {
    /// Whether the web accepts new instances at all.
    pub enabled: bool,
    /// How many submissions one address may make in an hour, whether they are
    /// accepted or not.
    pub per_ip_per_hour: u32,
}

impl Default for Submissions {
    fn default() -> Self {
        Self {
            enabled: true,
            per_ip_per_hour: 5,
        }
    }
}

/// `[notifications]`: telling the administrator what happens to the list.
///
/// Each channel is on when its address is set, and any number can be on at
/// once. All of them get the same events, those in `events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notifications {
    /// What to tell.
    pub events: Vec<Event>,
    /// A URL to POST each event to, as JSON.
    pub webhook_url: String,
    /// An ntfy topic's URL, as `https://ntfy.sh/my-topic`.
    pub ntfy_url: String,
    /// An ntfy access token, for a server that asks for one.
    pub ntfy_token: String,
    /// A Discord channel's webhook URL.
    pub discord_url: String,
    /// A Slack incoming webhook URL.
    pub slack_url: String,
    /// A Matrix homeserver, as `https://matrix.org`.
    pub matrix_homeserver: String,
    /// The Matrix room to write to, as `!abc123:matrix.org`.
    pub matrix_room: String,
    /// The access token of the Matrix account that writes.
    pub matrix_token: String,
    /// A Telegram bot's token, from BotFather.
    pub telegram_token: String,
    /// The chat the bot writes to: a user's, a group's or a channel's id, or
    /// a public channel's `@name`.
    pub telegram_chat_id: String,
    /// Telegram's Bot API, or a Bot API server of your own.
    pub telegram_api_url: String,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            events: vec![Event::InstanceAdded],
            webhook_url: String::new(),
            ntfy_url: String::new(),
            ntfy_token: String::new(),
            discord_url: String::new(),
            slack_url: String::new(),
            matrix_homeserver: String::new(),
            matrix_room: String::new(),
            matrix_token: String::new(),
            telegram_token: String::new(),
            telegram_chat_id: String::new(),
            telegram_api_url: TELEGRAM_API.into(),
        }
    }
}

/// Telegram's own Bot API.
pub const TELEGRAM_API: &str = "https://api.telegram.org";

impl Notifications {
    /// The Bot API to send through: the one configured, or Telegram's when
    /// that is left empty.
    pub fn telegram_api(&self) -> &str {
        match self.telegram_api_url.trim().trim_end_matches('/') {
            "" => TELEGRAM_API,
            url => url,
        }
    }
}

/// Something worth telling the administrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// Someone added an instance from the site.
    InstanceAdded,
    /// An instance has not answered for long enough to go offline.
    InstanceOffline,
    /// An offline instance answered again.
    InstanceOnline,
    /// An instance was offline for long enough to be deleted.
    InstanceDeleted,
}

impl Event {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InstanceAdded => "instance_added",
            Self::InstanceOffline => "instance_offline",
            Self::InstanceOnline => "instance_online",
            Self::InstanceDeleted => "instance_deleted",
        }
    }
}

impl Config {
    /// Reads a file and lays the environment over it.
    ///
    /// A missing file is an error only when `required` is set, which is the case
    /// when somebody named it. Otherwise the defaults and the environment are a
    /// complete configuration, which is what a container wants.
    pub fn load(path: &Path, required: bool) -> Result<Self> {
        let (text, source) = match std::fs::read_to_string(path) {
            Ok(text) => (text, Some(path.to_path_buf())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => {
                (String::new(), None)
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut config =
            Self::parse_with_env(&text, std::env::vars()).with_context(|| match &source {
                Some(path) => format!("in {} or the environment", path.display()),
                None => "in the environment".to_string(),
            })?;
        config.source = source;
        Ok(config)
    }

    /// Parses TOML text, without the environment.
    pub fn parse(text: &str) -> Result<Self> {
        Self::parse_with_env(text, std::iter::empty())
    }

    /// Parses TOML text and lays these variables over it.
    pub fn parse_with_env(
        text: &str,
        vars: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self> {
        let mut table: Table = toml::from_str(text)?;
        let environment = apply_env(&mut table, vars)?;
        let mut config: Self = Value::Table(table).try_into()?;
        config.environment = environment;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let c = &self.collector;
        for (key, value) in [
            ("interval_minutes", c.interval_minutes),
            ("days_active", c.days_active),
            ("concurrency", c.concurrency),
            ("timeout_seconds", c.timeout_seconds),
            ("max_response_mb", c.max_response_mb),
            ("offline_after_days", c.offline_after_days),
            ("delete_after_days", c.delete_after_days),
        ] {
            if value == 0 {
                bail!("[collector] {key} has to be at least 1");
            }
        }
        let n = &self.notifications;
        let matrix = [&n.matrix_homeserver, &n.matrix_room, &n.matrix_token];
        let set = matrix
            .iter()
            .filter(|value| !value.trim().is_empty())
            .count();
        if set != 0 && set != matrix.len() {
            bail!(
                "[notifications] Matrix needs matrix_homeserver, matrix_room and matrix_token, \
                 all three"
            );
        }
        if n.telegram_token.trim().is_empty() != n.telegram_chat_id.trim().is_empty() {
            bail!("[notifications] Telegram needs telegram_token and telegram_chat_id, both");
        }
        Ok(())
    }

    /// The `MESHVIEW_PLANET_` variables that set keys, by name: what to log at
    /// start, without their values.
    pub fn environment_variables(&self) -> impl Iterator<Item = &str> {
        self.environment.iter().map(String::as_str)
    }

    /// The file this came from, or none when it is defaults and environment only.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Where the web server listens.
    pub fn listen_addr(&self) -> Result<SocketAddr> {
        let ip = match self.server.bind.trim() {
            "" | "*" => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            bind => bind
                .parse()
                .with_context(|| format!("[server] bind is not an IP address: {bind}"))?,
        };
        Ok(SocketAddr::new(ip, self.server.port))
    }
}

impl Collector {
    pub fn interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.interval_minutes) * 60)
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.timeout_seconds))
    }

    pub fn max_response_bytes(&self) -> usize {
        self.max_response_mb as usize * 1024 * 1024
    }
}

/// The first of these files that exists.
pub fn first_existing<'a>(paths: &[&'a Path]) -> Option<&'a Path> {
    paths.iter().copied().find(|path| path.exists())
}

/// Lays `MESHVIEW_PLANET_<SECTION>__<KEY>` variables over a parsed file, and
/// answers the names of those it laid.
///
/// Each value is read as the type its key has, which the defaults say. Unknown
/// sections and keys are put in anyway, so that reading the result refuses
/// them as it would in the file.
fn apply_env(
    table: &mut Table,
    vars: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<String>> {
    let mut applied = Vec::new();
    let defaults = Table::try_from(Config::default()).expect("the defaults serialise");

    for (name, raw) in vars {
        let Some(rest) = name.strip_prefix(ENV_PREFIX) else {
            continue;
        };
        let Some((section, key)) = rest.split_once(ENV_SEPARATOR) else {
            continue;
        };
        if section.is_empty() || key.is_empty() {
            continue;
        }
        let (section, key) = (section.to_lowercase(), key.to_lowercase());

        let expected = defaults
            .get(&section)
            .and_then(Value::as_table)
            .and_then(|keys| keys.get(&key));
        let value = coerce(&raw, expected).with_context(|| format!("{name}={raw:?}"))?;

        let entry = table
            .entry(section.clone())
            .or_insert_with(|| Value::Table(Table::new()));
        let Value::Table(keys) = entry else {
            bail!("{name}: [{section}] is not a section");
        };
        keys.insert(key, value);
        applied.push(name);
    }
    Ok(applied)
}

/// Reads a value from the environment as the type of `expected`.
fn coerce(raw: &str, expected: Option<&Value>) -> Result<Value> {
    let literal = || {
        toml::from_str::<Table>(&format!("v = {raw}"))
            .ok()
            .and_then(|mut table| table.remove("v"))
    };

    Ok(match expected {
        Some(Value::String(_)) => Value::String(raw.to_string()),
        Some(Value::Boolean(_)) => Value::Boolean(match raw.trim().to_lowercase().as_str() {
            "1" | "yes" | "true" | "on" => true,
            "0" | "no" | "false" | "off" => false,
            _ => bail!("not a boolean"),
        }),
        Some(Value::Integer(_)) => match literal() {
            Some(value @ Value::Integer(_)) => value,
            _ => bail!("not a whole number"),
        },
        _ => literal().unwrap_or_else(|| Value::String(raw.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../sample.config.toml");

    fn env(vars: &[(&str, &str)]) -> Vec<(String, String)> {
        vars.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_sample_is_the_defaults() {
        assert_eq!(Config::parse(SAMPLE).unwrap(), Config::default());
    }

    #[test]
    fn nothing_at_all_is_a_configuration() {
        let config = Config::parse("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(
            config.listen_addr().unwrap(),
            "0.0.0.0:8080".parse().unwrap()
        );
        assert_eq!(config.collector.interval(), Duration::from_secs(4 * 3600));
    }

    #[test]
    fn unknown_keys_and_sections_are_refused() {
        assert!(Config::parse("[collector]\nintervall_minutes = 5").is_err());
        assert!(Config::parse("[colector]\ninterval_minutes = 5").is_err());
    }

    #[test]
    fn events_are_named_and_matrix_is_all_or_nothing() {
        let config = Config::parse_with_env(
            "",
            env(&[(
                "MESHVIEW_PLANET_NOTIFICATIONS__EVENTS",
                r#"["instance_added", "instance_offline"]"#,
            )]),
        )
        .unwrap();
        assert_eq!(
            config.notifications.events,
            [Event::InstanceAdded, Event::InstanceOffline]
        );
        assert!(Config::parse("[notifications]\nevents = [\"instance_exploded\"]").is_err());
        let error = Config::parse("[notifications]\nmatrix_homeserver = \"https://matrix.org\"")
            .unwrap_err();
        assert!(error.to_string().contains("all three"));
        let error = Config::parse("[notifications]\ntelegram_token = \"123:abc\"").unwrap_err();
        assert!(error.to_string().contains("both"));
    }

    #[test]
    fn telegram_s_own_api_unless_another_is_given() {
        let config =
            Config::parse("[notifications]\ntelegram_token = \"1:a\"\ntelegram_chat_id = \"2\"")
                .unwrap();
        assert_eq!(
            config.notifications.telegram_api(),
            "https://api.telegram.org"
        );
        let config = Config::parse_with_env(
            "",
            env(&[("MESHVIEW_PLANET_NOTIFICATIONS__TELEGRAM_API_URL", "")]),
        )
        .unwrap();
        assert_eq!(
            config.notifications.telegram_api(),
            "https://api.telegram.org"
        );
        let config =
            Config::parse("[notifications]\ntelegram_api_url = \"http://10.0.0.5:8081/\"").unwrap();
        assert_eq!(config.notifications.telegram_api(), "http://10.0.0.5:8081");
    }

    #[test]
    fn zero_is_refused_where_it_means_nothing() {
        let error = Config::parse("[collector]\nconcurrency = 0").unwrap_err();
        assert!(error.to_string().contains("concurrency"));
    }

    #[test]
    fn the_environment_wins_and_is_read_as_the_key_s_type() {
        let config = Config::parse_with_env(
            "[server]\nport = 1",
            env(&[
                ("MESHVIEW_PLANET_SERVER__PORT", "4241"),
                ("MESHVIEW_PLANET_COLLECTOR__ALLOW_PRIVATE_ADDRESSES", "yes"),
                ("MESHVIEW_PLANET_DATABASE__PATH", "/data/planet.db"),
                ("UNRELATED", "x"),
            ]),
        )
        .unwrap();
        assert_eq!(config.server.port, 4241);
        assert!(config.collector.allow_private_addresses);
        assert_eq!(config.database.path, PathBuf::from("/data/planet.db"));
        assert_eq!(config.environment_variables().count(), 3);
    }

    /// Every key there is, by section, from the defaults.
    fn every_key() -> std::collections::BTreeMap<String, Vec<String>> {
        let defaults = Table::try_from(Config::default()).unwrap();
        defaults
            .into_iter()
            .map(|(section, keys)| {
                let keys = match keys {
                    Value::Table(keys) => keys.keys().cloned().collect(),
                    _ => Vec::new(),
                };
                (section, keys)
            })
            .collect()
    }

    /// The documented keys, by section: the rows of each section's table in
    /// docs/configuration.md.
    fn documented_keys() -> std::collections::BTreeMap<String, Vec<String>> {
        const DOCS: &str = include_str!("../docs/configuration.md");
        let mut documented = std::collections::BTreeMap::new();
        for part in DOCS.split("\n### `[").skip(1) {
            let (section, body) = part.split_once("]`").unwrap();
            let body = body.split("\n## ").next().unwrap();
            let keys = body
                .lines()
                .filter_map(|line| line.strip_prefix("| `"))
                .filter_map(|line| line.split_once("` |"))
                .map(|(key, _)| key.to_string())
                .collect();
            documented.insert(section.to_string(), keys);
        }
        documented
    }

    #[test]
    fn the_documentation_has_every_key_and_no_other() {
        let every = every_key();
        let documented = documented_keys();
        assert_eq!(
            every.keys().collect::<Vec<_>>(),
            documented.keys().collect::<Vec<_>>(),
            "docs/configuration.md and the code disagree about the sections"
        );
        for (section, keys) in &every {
            let mut keys = keys.clone();
            let mut written = documented[section].clone();
            keys.sort();
            written.sort();
            assert_eq!(
                keys, written,
                "docs/configuration.md and the code disagree about [{section}]"
            );
        }
    }

    #[test]
    fn a_bad_value_in_the_environment_names_the_variable() {
        let error = Config::parse_with_env("", env(&[("MESHVIEW_PLANET_SERVER__PORT", "many")]))
            .unwrap_err();
        assert!(format!("{error:#}").contains("MESHVIEW_PLANET_SERVER__PORT"));
    }
}
