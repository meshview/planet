# Deployment

Meshview Planet is one process: it serves the web and collects from the
instances, writing one SQLite file. There is nothing else to run beside it.

## Commands

| Command | |
|---|---|
| `meshview-planet` or `meshview-planet run` | Serve the web and collect, until stopped. |
| `meshview-planet import <url>` | Add the instances another site lists at `<url>`, its `/api/instances`, and exit. |
| `meshview-planet collect` | One collection round now, with its cleanup, and exit. |
| `meshview-planet migrate` | Create the database or bring its schema up to date, and exit. `run` does this by itself. |

`--config <file>` (or `MESHVIEW_PLANET_CONFIG`) names the configuration; see
[Configuration](configuration.md).

## First start

```sh
meshview-planet   # serve and collect
```

The site starts empty, and fills as instances are added from its instances
page.

It can be seeded instead from a site that already lists Meshview instances, in
Meshview World's shape, before the first start:

```sh
meshview-planet import https://planet.example.org/api/instances
```

The import maps each instance listed to an area from the list (the original
kept free text), by its URL, from the instances known when this was written.
It does not ask the instances anything: the first collection round, at start
(as none has run yet), does. One the import does not know an area for is left
out with a warning, to be added from the web like any other. Running it twice adds nothing twice.

## The binary

The releases carry a static binary for Linux, x86_64 and aarch64: it brings
its own libc, SQLite and TLS roots, and runs on any distribution with nothing
installed. It writes only its database, and the files SQLite keeps beside it
while it runs, which it folds back in and removes when it stops.

A systemd unit, as an example:

```ini
[Unit]
Description=Meshview Planet
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/meshview-planet
WorkingDirectory=/var/lib/meshview-planet
DynamicUser=yes
StateDirectory=meshview-planet
ConfigurationDirectory=meshview-planet
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

With `DynamicUser`, the database is `/var/lib/meshview-planet/planet.db` and the
configuration, if any, `/etc/meshview-planet/config.toml`, which is where the
program looks when none is named.

## The container image

`ghcr.io/meshview/planet`, for amd64 and arm64: `latest` is the latest release,
a version tag (`1.0.0`) that release, and `master` the latest commit.

```sh
podman run -d --name planet -p 8080:8080 \
  -v planet-data:/data \
  ghcr.io/meshview/planet:latest
```

- It runs as user 10001, never root. `/data` is its working directory and where
  the database goes; it has to be writable by that user.
- The configuration, if any, goes in `/etc/meshview-planet/config.toml`, mounted
  read only. Environment variables are enough for most settings:
  `-e MESHVIEW_PLANET_COLLECTOR__INTERVAL_MINUTES=120`.
- There is no `VOLUME`: name where the data lives, or it goes with the
  container.
- The import runs as a command of its own:
  `podman run --rm -v planet-data:/data ghcr.io/meshview/planet import <url>`.

Building the image yourself:

```sh
podman build \
  --build-arg GIT_REVISION=$(git rev-parse HEAD) \
  --build-arg GIT_DATE=$(git show -s --format=%cs HEAD) \
  -t meshview-planet .
```

## Behind a reverse proxy

HTTPS is a reverse proxy's to serve: nginx, Caddy, Traefik or a Kubernetes
ingress in front, sending everything to the port. Set `[server] behind_proxy =
true` there, so that the limit on submissions counts the visitor's address, from
`X-Forwarded-For`, rather than the proxy's. Only then: a server answering
directly with it set lets anyone choose the address they are counted as.

The answers are already compressed and carry their own `ETag` and
`Cache-Control`; a proxy should pass them through as they are. With nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
}
```

## Notifications

The administrator can be told when an instance is added from the site, and,
if they want, when one goes offline, comes back or is deleted: by a webhook
with JSON, ntfy, Discord, Slack or a Matrix room, any number at once. See
[`[notifications]`](configuration.md#notifications). The log says at start
which channels are on and for what. Notices still on their way when the
process stops get a few seconds to go.

## Probes and stopping

`/livez` answers 200 for as long as the process can answer. The database is
opened and migrated before the server listens, so a process that answers is
ready.

SIGINT and SIGTERM stop it: a collection round in progress is abandoned (each
instance's data is written in a transaction of its own, so nothing is left
half written), requests in flight get five seconds to finish, and the database
is closed cleanly, which leaves the one file and nothing beside it.

## What it reaches

The server reaches out to the instances, over HTTP and HTTPS, and to the
notification channels configured, if any. To read an instance it never
connects to a private, loopback, link-local or otherwise not public address
(unless `allow_private_addresses` is set), checking the addresses a name
resolves to at the moment it connects and every redirect, so a submitted URL
cannot make it reach into the network it runs in. The notification channels
are the administrator's own, and may be on a private network.

The pages load their scripts, styles and fonts from the server itself. The map
tiles come from OpenStreetMap, and the API reference page loads Scalar from
jsDelivr.

## Backups

The data is the list of instances, what they answered latest, and their daily
history; everything but the history comes back by itself after a round. To
keep a copy, while it runs:

```sh
sqlite3 /data/planet.db "VACUUM INTO '/backups/planet-$(date +%F).db'"
```

`VACUUM INTO` makes a complete, consistent copy without stopping anything.
Copying the file itself while it runs may miss the latest writes.

## Upgrading

Stop the old version and start the new one on the same database: it brings the
schema up to date by itself. A database migrated by a newer version is not for
an older one; keep a backup from before an upgrade if you may go back.
