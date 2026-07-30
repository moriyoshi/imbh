# Runtime image for the imbh reference server (`imbhd`) plus the read-only terminal explorer
# (`imbh-tui`). Published to ghcr.io/moriyoshi/imbh by .github/workflows/release.yml on every `v*`
# tag; buildable locally with ../scripts/build-image.sh.
#
# NOT to be confused with crates/imbh-server/docker-plugin/Dockerfile, which builds the rootfs for
# the Docker *logging-driver plugin* — a different artifact with a different entrypoint contract and
# its own lifecycle (`docker plugin install`, not `docker run`). See docs/DOCKER_LOG_DRIVER.md.
#
# This image COMPILES NOTHING. It assembles binaries the release matrix already built and gated, so
# a multi-arch manifest costs no emulated Rust build — the workspace's fat-LTO release profile takes
# tens of minutes natively and hours under QEMU. The build context is therefore a small staged
# directory, *not* the repository root:
#
#   <context>/
#     linux/amd64/{imbhd,imbh-tui}
#     linux/arm64/{imbhd,imbh-tui}      # only the arches you are building
#     LICENSE
#     THIRD-PARTY-NOTICES.txt

# `prep` exists purely so the image needs no RUN step on the *target* platform: it is pinned to
# $BUILDPLATFORM, so `useradd` executes natively even when producing the linux/arm64 leg on an amd64
# builder. That is what keeps QEMU/binfmt out of the release pipeline entirely. It must stay the same
# base image as the final stage, because its /etc/passwd and /etc/group are copied over verbatim.
FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS prep
RUN useradd --system --uid 10001 --user-group \
      --home-dir /var/lib/imbh --shell /usr/sbin/nologin imbh \
 && install -d -o 10001 -g 10001 /staging/var/lib/imbh

FROM debian:bookworm-slim

# Release binaries are built on ubuntu-22.04 runners (glibc 2.35) and bookworm ships glibc 2.36, so
# the dynamic link resolves. Do NOT move this base image backwards (e.g. to bullseye/glibc 2.31)
# without moving the release runners back with it — see the `build` job in release.yml.
COPY --from=prep /etc/passwd /etc/group /etc/
# Carries the ownership, not the mode: `COPY <dir> <dir>` copies the source directory's *contents*, so
# the destination is created at BuildKit's default 0755. Ownership is what matters here — it is what
# lets the unprivileged runtime user create the WAL and segments (and what the declared VOLUME
# inherits when no bind mount is supplied).
COPY --from=prep --chown=10001:10001 /staging/var/lib/imbh /var/lib/imbh

# TARGETARCH is supplied per-platform by BuildKit (`amd64` / `arm64`) and selects the prebuilt binary
# for this leg of the manifest.
ARG TARGETARCH
ARG TARGETOS=linux
COPY ${TARGETOS}/${TARGETARCH}/imbhd    /usr/local/bin/imbhd
COPY ${TARGETOS}/${TARGETARCH}/imbh-tui /usr/local/bin/imbh-tui

# Apache-2.0 §4(d): the attribution notices travel with every binary distribution, this image
# included (see README.md "License").
COPY LICENSE THIRD-PARTY-NOTICES.txt /usr/share/doc/imbh/

# Loopback is the right default for the bare binary but useless in a container, so bind both
# listeners to all interfaces here. `IMBH_FLUSH` (default `interval=5s`) and
# `IMBH_MAINTENANCE_INTERVAL` (default `60s`) are deliberately left unset so the image inherits
# whatever the binary's own defaults are rather than pinning a second copy of them.
ENV IMBH_LISTEN_ADDR=0.0.0.0:4318 \
    IMBH_GRPC_LISTEN_ADDR=0.0.0.0:4317
EXPOSE 4318 4317

# The database is the whole point of the process, so declare it a volume: `docker run` without an
# explicit `-v` then keeps the data in an anonymous volume instead of the container's writable layer.
VOLUME /var/lib/imbh

LABEL org.opencontainers.image.source="https://github.com/moriyoshi/imbh" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.title="imbh" \
      org.opencontainers.image.description="imbhd (reference OTLP server) + imbh-tui, for the embeddable observability database imbh"

USER 10001:10001
# `imbhd [DB_DIR] [ADDR] [GRPC_ADDR]`; CMD supplies only DB_DIR so the ENV addresses above apply.
# Override the whole thing for the explorer:
#   docker run -it --rm -v imbh-data:/var/lib/imbh --entrypoint imbh-tui <image> /var/lib/imbh
ENTRYPOINT ["/usr/local/bin/imbhd"]
CMD ["/var/lib/imbh"]
