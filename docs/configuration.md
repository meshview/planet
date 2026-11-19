# Configuration

Meshview Rust Planet is configured with a TOML file, environment variables, or both.
Every key has a default, so no file at all is a valid configuration: an empty
database and the defaults are a working site that lists no instances yet.

## Where it is read from

| How | File | If the file is missing |
|---|---|---|
| `--config <file>` | that file | error |
| `MESHVIEW_PLANET_CONFIG=<file>` | that file | error |
| neither | `./config.toml`, or else `/etc/meshview-planet/config.toml` | defaults and environment only |

Without a file named, the working directory comes first and the system's place
second: the first of the two that exists is read, and the log says which.

The file is only ever read, never written, so it can live where the process
cannot write: on a read-only mount, owned by root, while the database directory
is the one place the process writes. In the container image that is
`/etc/meshview-planet`, apart from the data in `/data`.

## The format

Strings in quotes, booleans as `true`/`false`, numbers bare.
[sample.config.toml](../sample.config.toml) has every key, at its default.

Anything the program does not know is an error, whether it is a section or a
key, and so is a value of the wrong type or a period of zero:

```text
Error: in config.toml or the environment

Caused by:
    unknown field `intervall_minutes`, expected one of `interval_minutes`, `days_active`, ...
    in `collector`
```

## Environment variables

Any key can be set from the environment, and the environment wins over the
file. The name is built from the section and the key:

```text
MESHVIEW_PLANET_<SECTION>__<KEY>
```

- `MESHVIEW_PLANET_` is the prefix.
- Between the section and the key go **two** underscores, since both may have
  one of their own.
- Upper or lower case, as you like.

The value is read as the type the key has: `MESHVIEW_PLANET_SERVER__PORT=8080` is
a number, `MESHVIEW_PLANET_SUBMISSIONS__ENABLED=false` a boolean (`true`, `yes`,
`on`, `1`, or their opposites) and `MESHVIEW_PLANET_DATABASE__PATH=/data/planet.db`
a path. The log says which variables were used, by name, at start.

## Reference

### `[server]`

| Key | Default | |
|---|---|---|
| `bind` | `"*"` | The address to listen on. `*` is every interface, IPv4; `::` every one, IPv6 and IPv4. |
| `port` | `8080` | The port. |
| `domain` | `""` | The address the site is reached at, as `planet.example.org` (HTTPS is assumed unless it starts with `http://`). Link previews, the card a chat or social network shows for a shared link, take their picture from it, `/static/og.png`, as they need an absolute address. Empty, it is taken from each request's `Host` (and, with `behind_proxy`, `X-Forwarded-Proto`). It does not configure binding or TLS. |
| `behind_proxy` | `false` | Take the client's address from the last entry of `X-Forwarded-For`, for the submission limit. Only behind a reverse proxy that sets it: anyone can send the header to a server that answers directly, and so choose the address they are counted as. |

### `[site]`

| Key | Default | |
|---|---|---|
| `map_kiosk_lock` | `false` | The map's kiosk mode (`/?kiosk`), the map alone on the whole screen, without its full screen button, so that nothing on the screen leaves it: for a kiosk proper. |

### `[database]`

| Key | Default | |
|---|---|---|
| `path` | `"planet.db"` | The SQLite file, relative to the working directory or absolute. It is created, with its schema, if it is not there, and migrated on start. |

### `[collector]`

| Key | Default | |
|---|---|---|
| `interval_minutes` | `240` | Minutes between collection rounds. They count from the last round in the database, so a restart does not bring one forward: the first round after a start runs at once only if the interval since the last one is over (or there was none). |
| `days_active` | `14` | The nodes asked of each instance: those it heard in this many days. The map's default period is the same. |
| `concurrency` | `8` | How many instances are read at the same time. |
| `timeout_seconds` | `60` | Seconds each request to an instance gets, its answer included. |
| `max_response_mb` | `64` | The largest answer read from an instance, in megabytes. A larger one is a failed collection. |
| `offline_after_days` | `3` | Days without a good collection after which an instance is offline: last in the list, and its nodes off the map until it answers again. Counted from when it was added, for one that never answered. |
| `delete_after_days` | `30` | Days offline after which an instance is deleted, with its nodes and its history. |
| `allow_private_addresses` | `false` | Whether instances on loopback, private, link-local or otherwise not public addresses may be read and submitted. Only for testing: on a public server it lets anyone make it reach into the network it runs in. |

### `[submissions]`

| Key | Default | |
|---|---|---|
| `enabled` | `true` | Whether the site accepts new instances. Off, the form is replaced by a note and the API answers 403. |
| `per_ip_per_hour` | `5` | Submissions one address may make in an hour, accepted or not. IPv6 addresses count by their /64. |

### `[notifications]`

Telling the administrator what happens to the list. Each channel is on when
its address is set, and any number of them can be on at once; all get the same
events. Notices are sent in the background: a channel that is down never holds
up a submission or a collection, and its error is only logged. Unlike the
instances, the channels may be on a private network. `meshview-planet notify`
sends a test notice to each, to check them: the webhook gets it as the event
`test`, with every field a real one has, about an example instance.

| Key | Default | |
|---|---|---|
| `events` | `["instance_added"]` | What to tell: any of `instance_added` (from the site, with the address it came from), `instance_offline`, `instance_online` (answering again after being offline) and `instance_deleted`. |
| `webhook_url` | `""` | A URL to POST each event to, as JSON: `event`, `title`, `message`, `at`, `days`, `from_ip`, and `instance` with its `url`, `country`, `region`, `area` (`en`, `es`), `version`, `node_count` and `last_error`. |
| `ntfy_url` | `""` | An [ntfy](https://ntfy.sh) topic, as `https://ntfy.sh/my-topic` or on a server of your own. The notice opens the instance when tapped. |
| `ntfy_token` | `""` | An ntfy access token, for a server or topic that asks for one. |
| `discord_url` | `""` | A Discord channel's webhook URL. |
| `slack_url` | `""` | A Slack incoming webhook URL. |
| `matrix_homeserver` | `""` | A Matrix homeserver, as `https://matrix.org`. Matrix needs this and the two below, all three. |
| `matrix_room` | `""` | The id of the room to write to, as `!abc123:matrix.org`, which the account has joined. |
| `matrix_token` | `""` | The access token of the account that writes. |
| `telegram_token` | `""` | A Telegram bot's token, from [BotFather](https://t.me/BotFather). Telegram needs this and `telegram_chat_id`, both. |
| `telegram_chat_id` | `""` | The chat the bot writes to: a user's, a group's or a channel's id (`-100…` for groups and channels), or a public channel's `@name`. The bot has to be in it, or have been started by the user. |
| `telegram_api_url` | `"https://api.telegram.org"` | Telegram's Bot API, or a [Bot API server](https://github.com/tdlib/telegram-bot-api) of your own. Left out or empty, Telegram's. |

A notice, as ntfy shows it:

```text
New instance: Galicia, Spain
https://meshview.example.org
Meshview 5.0.0, 812 nodes. Added from the site, from 203.0.113.7.
```
