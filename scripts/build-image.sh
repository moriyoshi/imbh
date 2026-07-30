#!/usr/bin/env bash
# Build the distribution container image (docker/Dockerfile) locally, for the host architecture.
#
# This is the local half of the image story, and the only way to exercise docker/Dockerfile without
# pushing a version tag. It compiles both shipped binaries with the release feature set, stages the
# build context the Dockerfile expects, and builds.
#
# The multi-arch release image is built by .github/workflows/release.yml's `image` job, which stages
# the same context inline from the archives the build matrix produced -- it does not call this script,
# so that job stays readable on its own. Both satisfy the BUILD CONTEXT CONTRACT documented in
# docker/Dockerfile's header; that block is the source of truth, and a layout change has to be applied
# here and there.
#
# Usage:
#   ./scripts/build-image.sh                              # -> imbh:dev
#   IMAGE=ghcr.io/moriyoshi/imbh:0.1.1 ./scripts/build-image.sh
#   DOCKER="sudo docker" ./scripts/build-image.sh          # rootful daemon
#   FEATURES=grpc,tracing ./scripts/build-image.sh         # trim the imbhd feature set
#
# Like the other scripts here it degrades gracefully (prints a hint, exits 0) when the tool is
# absent, so an offline/containerless dev environment never breaks the gate.
set -euo pipefail
cd "$(dirname "$0")/.."

: "${IMAGE:=imbh:dev}"
# Deliberately unquoted at the call sites so DOCKER="sudo docker" word-splits, matching
# crates/imbh-server/docker-plugin/build.sh.
: "${DOCKER:=docker}"
# The release feature set, mirroring release.yml's Linux legs. `docker` (log-driver plugin API) and
# `tracing` (stderr diagnostics) add no crate and ~5 crates respectively; `grpc` is what makes
# OTLP/gRPC on 4317 -- the default transport for most OTel SDKs -- work at all. All three are off by
# default so the footprint gate measures the minimal graph, so a shipping build has to name them.
: "${FEATURES:=docker,grpc,tracing}"

if ! command -v "${DOCKER%% *}" > /dev/null 2>&1; then
  echo "build-image: ${DOCKER%% *} not found -- skipping image build."
  echo "  Install Docker, then re-run: ./scripts/build-image.sh"
  exit 0
fi

if [ ! -f THIRD-PARTY-NOTICES.txt ]; then
  echo "build-image: THIRD-PARTY-NOTICES.txt is missing -- it must ship in the image (Apache-2.0 §4(d))." >&2
  echo "  Generate it first (networked env): ./scripts/gen-notices.sh" >&2
  exit 1
fi

# docker/Dockerfile selects the binary with BuildKit's ${TARGETARCH}, whose values are Go-style, so
# translate uname's spelling to match.
case "$(uname -m)" in
  x86_64 | amd64) arch=amd64 ;;
  aarch64 | arm64) arch=arm64 ;;
  *)
    echo "build-image: unsupported host architecture $(uname -m)" >&2
    exit 1
    ;;
esac

echo "build-image: cargo build --release (imbhd features: ${FEATURES}) ..."
cargo build --release -p imbh-server --features "${FEATURES}"
cargo build --release -p imbh-tui

# Under target/ so it inherits .gitignore; the context root is this directory, so the repo-root
# .dockerignore (which excludes target/) does not apply to it.
ctx="target/image-ctx"
rm -rf "${ctx}"
mkdir -p "${ctx}/linux/${arch}"
install -m 0755 "target/release/imbhd" "${ctx}/linux/${arch}/imbhd"
install -m 0755 "target/release/imbh-tui" "${ctx}/linux/${arch}/imbh-tui"
# Apache-2.0 §4(d): the notices travel with every binary distribution, the image included.
install -m 0644 LICENSE "${ctx}/LICENSE"
install -m 0644 THIRD-PARTY-NOTICES.txt "${ctx}/THIRD-PARTY-NOTICES.txt"

echo "build-image: docker build -> ${IMAGE} (linux/${arch}) ..."
# BuildKit is required, not merely preferred: ${TARGETARCH} and `FROM --platform=$BUILDPLATFORM` are
# BuildKit features and silently expand to nothing under the legacy builder.
DOCKER_BUILDKIT=1 ${DOCKER} build \
  -f docker/Dockerfile \
  -t "${IMAGE}" \
  "${ctx}"

echo "build-image: built ${IMAGE}"
echo "  serve:   ${DOCKER} run --rm -p 4318:4318 -p 4317:4317 -v imbh-data:/var/lib/imbh ${IMAGE}"
echo "  explore: ${DOCKER} run -it --rm -v imbh-data:/var/lib/imbh --entrypoint imbh-tui ${IMAGE} /var/lib/imbh"
