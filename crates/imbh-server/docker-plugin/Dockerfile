# Rootfs for the imbh Docker logging-driver plugin (docs/DOCKER_LOG_DRIVER.md).
#
# Build context is the repository root:
#   docker build -f crates/imbh-server/docker-plugin/Dockerfile -t imbh-log-driver-rootfs .
# `build.sh` next to this file does that, exports the rootfs, and registers the plugin.

FROM rust:1-alpine AS build
# musl-dev/clang: the C toolchain zstd-sys (via parquet) and the ring-free TLS-less graph need.
RUN apk add --no-cache musl-dev clang lld
WORKDIR /src
COPY . .
# `docker` is the plugin API. `grpc` matters more than it looks: OTLP/gRPC on 4317 is the *default*
# transport for most OTel SDKs, so without it an app container pointed at this plugin fails until
# someone works out that it has to say `http/protobuf`. `tracing` sends imbhd's own diagnostics to
# stderr, which the Docker daemon log captures. None are on by default (ARCHITECTURE.md §11).
RUN cargo build --release -p imbh-server --features docker,grpc,tracing

FROM alpine:3
# `/run/docker/plugins` holds the plugin socket; `/run/docker/logging` is where the daemon creates
# each container's FIFO inside this rootfs; `/var/lib/imbh` is the database (bind-mounted from the
# host by the `data` mount in config.json).
RUN mkdir -p /run/docker/plugins /run/docker/logging /var/lib/imbh
COPY --from=build /src/target/release/imbhd /usr/bin/imbhd
ENTRYPOINT ["/usr/bin/imbhd"]
