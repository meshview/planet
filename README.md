# Meshview Planet

A world map of the nodes every [Meshview](https://github.com/meshview/meshview)
instance sees, and the list of those instances: a small application,
to deploy wherever you want.

It reads every listed instance every few hours (`/version` and its nodes API),
keeps what each one answered in SQLite, and counts its nodes day by day. An
instance that stops answering is marked offline after three days, which takes
its nodes off the map, and deleted after thirty more; one that answers again
comes back by itself.

Anyone can add an instance from the web, with no moderation: it is accepted
only if it answers as a Meshview does there and then, and its area is chosen
from a closed list of countries and their regions (ISO 3166), so nothing typed
by a visitor is ever shown as text.

## Running it

```sh
cargo build --release
./target/release/meshview-planet   # serve and collect
```

The configuration is `config.toml` (see `sample.config.toml`), or environment
variables as `MESHVIEW_PLANET_<SECTION>__<KEY>`. Other commands: `collect` (one
round now), `migrate`, and `--help`.

## Development

```sh
git config core.hooksPath .githooks   # rustfmt and clippy before each commit
cargo test
```

The list of areas, `assets/areas.json`, is built from Debian's
[iso-codes](https://salsa.debian.org/iso-codes-team/iso-codes) (LGPL-2.1 or
later) by `scripts/update-areas.py`.

## License

AGPL-3.0-or-later, as Meshview.
