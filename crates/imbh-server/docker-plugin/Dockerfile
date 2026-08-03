# Rootfs for the imbh Docker logging-driver plugin (docs/DOCKER_LOG_DRIVER.md).
#
# A managed plugin is NOT an image: it is a flattened rootfs plus the config.json next to this file,
# handed to `docker plugin create`. This Dockerfile only produces the filesystem — every piece of
# image metadata (ENTRYPOINT, ENV, USER, VOLUME) is discarded when the rootfs is flattened, and
# config.json is the sole source of truth for all of it. Do not add any here; it would look
# authoritative and do nothing.
#
# NOT to be confused with docker/Dockerfile, which builds the `ghcr.io/moriyoshi/imbh` runtime image
# — a different artifact with a different lifecycle (`docker run`, not `docker plugin install`).
#
# This COMPILES NOTHING, for the same reason docker/Dockerfile does not: it assembles a binary the
# release matrix already built and gated. The workspace's fat-LTO release profile takes tens of
# minutes natively and hours under QEMU, and the arm64 rootfs has to be produced from an amd64
# runner. There is also no RUN step anywhere below — see the WORKDIR block — so no leg of this build
# ever executes a foreign-architecture instruction, and binfmt/QEMU stays out of the pipeline.
#
# ── BUILD CONTEXT CONTRACT ───────────────────────────────────────────────────────────────
# The context is a small staged directory, *not* the repository root:
#
#   <context>/
#     linux/amd64/imbhd
#     linux/arm64/imbhd                 # only the arches you are building
#     LICENSE
#     THIRD-PARTY-NOTICES.txt
#
# Deliberately the same shape as docker/Dockerfile's context (minus imbh-tui, which a plugin has no
# way to run), so the two staging steps stay recognizably one thing.
#
# This block is the contract, and it has TWO independent implementations on purpose:
#
#   ./build.sh                          local, single-arch: compiles for the host, then builds
#   release.yml's `plugin` job          release, per-arch: unpacks the archives, then pushes
#
# CI stages inline rather than shelling out to the script, matching how the `image` job treats
# ../../../scripts/build-image.sh. The cost is that both producers have to match this block -- so if
# you change the layout, change it here first and then update both. A `workflow_dispatch` dry run
# catches a CI-side mismatch; a bare `./build.sh` catches a local one.

# Release binaries are built on ubuntu-22.04 runners (glibc 2.35) and bookworm ships glibc 2.36, so
# the dynamic link resolves. Same base as docker/Dockerfile, deliberately. This was alpine/musl until
# the plugin got a published build; musl would have meant compiling a second time, since the release
# matrix is glibc — exactly the cost this file now exists to avoid. Do NOT move the default backwards
# without moving the release runners too — see the `build` job in release.yml.
#
# It is an ARG only for the LOCAL path: `build.sh` compiles against the *host's* glibc, so a dev
# machine newer than bookworm (Ubuntu 24.04 is glibc 2.39) can produce a binary this base cannot run —
# it depends on which symbols the graph actually references, so it is a hazard rather than a rule.
# `BASE=debian:trixie-slim ./build.sh` is the way out, and build.sh's smoke test is what tells you.
# CI never overrides it: a published plugin is always bookworm, over archive binaries whose glibc
# floor is known.
ARG BASE=debian:bookworm-slim
FROM ${BASE}

# TARGETARCH is supplied per-platform by BuildKit (`amd64` / `arm64`) and selects the prebuilt binary
# for the architecture being packaged. Unlike the image, each arch ships as its OWN plugin tag:
# managed plugins have no manifest-list support (moby's plugin fetch path resolves a single manifest
# and does no platform matching), so there is nothing for a multi-arch tag to point at.
ARG TARGETARCH
ARG TARGETOS=linux
COPY ${TARGETOS}/${TARGETARCH}/imbhd /usr/bin/imbhd

# Apache-2.0 §4(d): the attribution notices travel with every binary distribution, this rootfs
# included (see README.md "License"). `docker plugin inspect` does not surface them, so they sit at
# the path the runtime image uses for the same files.
COPY LICENSE THIRD-PARTY-NOTICES.txt /usr/share/doc/imbh/

# The directories the plugin runtime needs: `/run/docker/plugins` holds the socket it serves,
# `/run/docker/logging` is where the daemon creates each container's FIFO inside this rootfs, and
# `/var/lib/imbh` is the database (bind-mounted from the host by the `data` mount in config.json).
#
# WORKDIR and not `RUN mkdir -p`, because WORKDIR creates the directory as a builder-side metadata
# operation while RUN would execute a shell *for the target platform* — the one thing that would drag
# QEMU into the arm64 leg. The trailing `WORKDIR /` is cosmetic (a flattened rootfs carries no working
# directory; config.json's `workdir` is what applies) but leaves the image sane for a
# `docker run --entrypoint` poke at the same filesystem.
WORKDIR /run/docker/plugins
WORKDIR /run/docker/logging
WORKDIR /var/lib/imbh
WORKDIR /
