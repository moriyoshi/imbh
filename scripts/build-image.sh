#!/usr/bin/env bash
# Build the distribution container image (docker/Dockerfile) locally, for the host architecture.
#
# .github/workflows/release.yml builds a linux/amd64 + linux/arm64 manifest by staging the release
# matrix's artifacts; this script is the single-arch local equivalent, so the image is reproducible
# without a tag push. It compiles the two shipped binaries with the release feature set, stages the
# small build context docker/Dockerfile expects (see the layout comment there), and builds.
#
# Usage:
#   ./scripts/build-image.sh                              # -> imbh:dev
#   IMAGE=ghcr.io/moriyoshi/imbh:0.1.1 ./scripts/build-image.sh
#   DOCKER="sudo docker" ./scripts/build-image.sh          # rootful daemon
#
# Like the other scripts here it degrades gracefully (prints a hint, exits 0) when the tool is
# absent, so an offline/containerless dev environment never breaks the gate.
set -euo pipefail
cd "$(dirname "$0")/.."

: "${IMAGE:=imbh:dev}"
# Deliberately unquoted at the call sites so DOCKER="sudo docker" word-splits, matching
# crates/imbh-server/docker-plugin/build.sh.
: "${DOCKER:=docker}"
# The release feature set. `docker` (log-driver plugin API) and `tracing` (stderr diagnostics) add
# no crate and ~5 crates respectively; `grpc` is what makes OTLP/gRPC on 4317 — the default
# transport for most OTel SDKs — work at all. All three are off by default so the footprint gate
# measures the minimal graph, so a shipping build has to name them. Mirrors release.yml.
: "${FEATURES:=docker,grpc,tracing}"

if ! command -v "${DOCKER%% *}" >/dev/null 2>&1; then
  echo "build-image: ${DOCKER%% *} not found — skipping image build."
  echo "  Install Docker, then re-run: ./scripts/build-image.sh"
  exit 0
fi

if [ ! -f THIRD-PARTY-NOTICES.txt ]; then
  echo "build-image: THIRD-PARTY-NOTICES.txt is missing — it must ship in the image (Apache-2.0 §4(d))." >&2
  echo "  Generate it first (networked env): ./scripts/gen-notices.sh" >&2
  exit 1
fi

# docker/Dockerfile selects the binary by BuildKit's ${TARGETARCH}, whose values are Go-style.
case "$(uname -m)" in
  x86_64 | amd64) ARCH=amd64 ;;
  aarch64 | arm64) ARCH=arm64 ;;
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
CTX="target/image-ctx"
rm -rf "${CTX}"
mkdir -p "${CTX}/linux/${ARCH}"
install -m 0755 target/release/imbhd "${CTX}/linux/${ARCH}/imbhd"
install -m 0755 target/release/imbh-tui "${CTX}/linux/${ARCH}/imbh-tui"
install -m 0644 LICENSE "${CTX}/LICENSE"
install -m 0644 THIRD-PARTY-NOTICES.txt "${CTX}/THIRD-PARTY-NOTICES.txt"

echo "build-image: docker build -> ${IMAGE} (linux/${ARCH}) ..."
# BuildKit is required, not merely preferred: ${TARGETARCH} and `FROM --platform=$BUILDPLATFORM` are
# BuildKit features and silently expand to nothing under the legacy builder.
DOCKER_BUILDKIT=1 ${DOCKER} build \
  -f docker/Dockerfile \
  -t "${IMAGE}" \
  "${CTX}"

echo "build-image: built ${IMAGE}"
echo "  serve:   ${DOCKER} run --rm -p 4318:4318 -p 4317:4317 -v imbh-data:/var/lib/imbh ${IMAGE}"
echo "  explore: ${DOCKER} run -it --rm -v imbh-data:/var/lib/imbh --entrypoint imbh-tui ${IMAGE} /var/lib/imbh"
