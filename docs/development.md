# Development

## Layout

One crate, `meshview-planet`, a library and a binary: the library only so that
the integration tests under `tests/` can reach the pieces.

| Path | |
|---|---|
| `src/config.rs` | The configuration: TOML, the environment on top, and the defaults. |
| `src/db.rs` | Every SQL statement, and opening and migrating the database. |
| `migrations/` | The schema, as sqlx migrations, compiled into the binary. |
| `src/net.rs` | URLs, the HTTP client, and keeping it to public addresses. |
| `src/meshview.rs` | Reading an instance: its `/version` and its nodes, from either API. |
| `src/collector.rs` | The collection rounds and the cleanup. |
| `src/submit.rs` | Adding an instance from the web, and the limit per address. |
| `src/import.rs` | Seeding the list from another site, and the areas of its instances. |
| `src/admin.rs` | The `instance` and `areas` commands: managing instances from the command line. |
| `src/term.rs` | Colour for what the commands print, and how long ago things were. |
| `src/areas.rs` | The closed list of areas, from `assets/areas.json`. |
| `src/web/` | The API (`v1.rs`, `legacy.rs`), its reference (`reference.rs`), the cache of answers (`cache.rs`) and the pages (`pages.rs`). |
| `templates/` | The pages, as Tera 2 templates. |
| `assets/static/` | The pages' scripts, styles, pictures, and the libraries under `vendor/`. |
| `assets/lang/` | The pages' translations. |
| `assets/api/intro.md` | The reference's introduction. |
| `assets/api-lang/` | The reference's translations. |

Templates, files and translations are built into the binary, so changing any of
them takes a build and a restart.

## Tests

```sh
cargo test
```

The integration tests start pretend Meshview instances on local ports (Rust,
Python and "plus" ones, broken ones, ones that answer a web page, ones that
answer too much) and run the collector, the cleanup, submissions and the API
against them, with the clock passed in. Nothing reaches the Internet.

Some tests hold things that are easy to let slip:

- Every key of the configuration is in `docs/configuration.md`, and no other.
- Every string the templates and scripts translate is in `assets/lang/es.json`,
  and nothing else is.
- Every piece of the API reference is in `assets/api-lang/es.json`, and nothing
  else is. When one fails it prints what is missing as JSON, to translate and
  add.
- Every instance the import knows is in a real area, with its URL normalised.

The pre-commit hook in `.githooks/` runs rustfmt and clippy; turn it on once
per clone with `git config core.hooksPath .githooks`.

## Translations

The pages are written in English. A string to translate is `{{ "…" | tr }}` in a
template and `MeshI18n.t("…")` (or `MeshI18n.h`, escaped for HTML) in a script;
its translation goes in `assets/lang/<code>.json`, keyed by the English. A new
language is a new catalog there and its code in `LANGUAGES` in
`src/web/pages.rs`, plus a catalog for the reference in `assets/api-lang/`.

## Areas

`assets/areas.json` is built from Debian's
[iso-codes](https://salsa.debian.org/iso-codes-team/iso-codes): the countries of
ISO 3166-1, with their Spanish names, and the top level subdivisions of ISO
3166-2, with their own, local names. To update it, with a checkout or a release
of iso-codes unpacked:

```sh
python3 scripts/update-areas.py <iso-codes source tree>
```

An area stored in a database stays valid if a later list drops it: it is shown
by its codes.

## Libraries

The pages' libraries are under `assets/static/vendor/`, each in a directory
named after its version, with `manifest.json` recording where each came from
and its checksum; see its README. Most are Meshview's, at the same versions.

## CI and releases

`.github/workflows/ci.yml`, on every push to `master` and every pull request:

1. Formatting, clippy and the tests.
2. Static binaries for x86_64 and aarch64 (musl), checked to be static.
3. The container image, built from those binaries, started and asked a few
   things, stopped and checked to leave only its database; then published to
   GHCR for pushes, for both architectures.

A release is a tag, the version as Cargo writes it (`1.0.0`, no `v`), on a
commit whose `Cargo.toml` says that version; the tag's message becomes the
release notes. The run then also tags the image with the version and `latest`,
and publishes the binaries, as tarballs with their checksums, on a release
page.
