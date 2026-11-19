# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

# The versions this build rests on, written down once, here, so that the
# obvious thing works:
#
#   podman build -t meshview-planet .
#
# Both name a minor line rather than a patch, so fixes arrive without a commit.
ARG ALPINE_VERSION=3.24
ARG RUST_VERSION=1

# Which of the two stages below the binary comes from. Declared out here because
# global scope is the only one a FROM can read.
ARG BINARY_SOURCE=builder

# --- the binary, compiled here -----------------------------------------------

FROM docker.io/library/rust:${RUST_VERSION}-alpine${ALPINE_VERSION} AS builder

# gcc and musl-dev come with the base image, and they are all libsqlite3-sys
# needs to compile SQLite itself.
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY .cargo ./.cargo
COPY src ./src
COPY migrations ./migrations
COPY templates ./templates
COPY assets ./assets

# The commit, for /version, since the source is copied without .git:
#
#   podman build --build-arg GIT_REVISION=$(git rev-parse HEAD) \
#     --build-arg GIT_DATE=$(git show -s --format=%cs HEAD) -t meshview-planet .
ARG GIT_REVISION=""
ARG GIT_DATE=""

# No target is named: this image's own is a musl one, whichever architecture it
# runs on. The caches make a second build quick; they also mean the binary is
# produced inside a mount that does not survive the step, hence copying it out.
# What ships is one file that brings its own libc: a dynamically linked one
# would mean the ground had moved, and is refused here rather than found out in
# production.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    set -eux; \
    MESHVIEW_PLANET_GIT_REVISION="$GIT_REVISION" MESHVIEW_PLANET_GIT_DATE="$GIT_DATE" \
        cargo build --release --locked; \
    install -D target/release/meshview-planet /out/meshview-planet; \
    if readelf -l /out/meshview-planet | grep -q INTERP; then \
        echo "meshview-planet is not statically linked" >&2; \
        exit 1; \
    fi

# --- the binary, built somewhere better --------------------------------------

# CI compiles with a proper cargo cache and publishes the binary as an artifact;
# point the build at what it already made, under dist/:
#
#   podman build --build-arg BINARY_SOURCE=prebuilt -t meshview-planet .
#
# The name carries the architecture in the build's own vocabulary, which is
# what lets one multi-platform build pick each binary out of the same directory.
FROM scratch AS prebuilt
ARG TARGETARCH
COPY dist/meshview-planet-${TARGETARCH} /out/meshview-planet

# --- whichever of those two it was -------------------------------------------

# COPY --from takes no variables; a stage with a fixed name resolves it.
FROM ${BINARY_SOURCE} AS binary

# --- the image that ships ----------------------------------------------------

FROM docker.io/library/alpine:${ALPINE_VERSION}

LABEL org.opencontainers.image.title="Meshview Rust Planet" \
      org.opencontainers.image.description="A world map of the nodes every Meshview instance sees" \
      org.opencontainers.image.source="https://github.com/meshview/planet" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"

# The mode is set rather than inherited: a binary that has travelled through a
# CI artifact arrives without its executable bit.
COPY --from=binary --chmod=0755 /out/meshview-planet /usr/local/bin/meshview-planet

# Never root, and 10001, as Meshview's image, so that what it writes on a
# mounted volume belongs to nobody in particular. /data is where the database
# lives, and the working directory; the configuration goes in
# /etc/meshview-planet, read only, where the program looks for config.toml when
# none is named. The TLS roots are built into the binary, so the image needs no
# ca-certificates.
RUN addgroup -g 10001 meshview \
 && adduser -D -H -G meshview -u 10001 meshview \
 && mkdir -p /data /etc/meshview-planet \
 && chown meshview:meshview /data

# The database is the default one, planet.db in the working directory: /data.
# No VOLUME, which would only make an unnamed volume that goes away with the
# container: where the state lives is the operator's to decide and to name. No
# HEALTHCHECK either: /livez is there to be asked, by whoever runs it.
EXPOSE 8080

# By number, so that Kubernetes' runAsNonRoot can check it.
USER 10001:10001
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/meshview-planet"]
CMD ["run"]
