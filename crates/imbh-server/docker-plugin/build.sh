#!/bin/sh
# Build and register the imbh logging-driver plugin (docs/DOCKER_LOG_DRIVER.md) on the LOCAL daemon,
# for the host architecture.
#
# This is the local half of the plugin story. The published per-architecture plugins
# (`ghcr.io/moriyoshi/imbh-log-driver:X.Y.Z-amd64` / `-arm64`) are built by release.yml's `plugin`
# job, which stages the same context inline from the archives the build matrix produced -- it does
# not call this script, so that job stays readable on its own. Both satisfy the BUILD CONTEXT
# CONTRACT documented in the Dockerfile's header; that block is the source of truth, and a layout
# change has to be applied here and there.
#
# A managed plugin is a rootfs directory plus a config.json, handed to `plugin create`. There is no
# Dockerfile-to-plugin shortcut, so this compiles imbhd, builds a rootfs image around it, exports that
# filesystem, and pairs the two.
#
#   ./crates/imbh-server/docker-plugin/build.sh              # build + create
#   PLUGIN=me/imbh:dev ./…/build.sh                          # under another name
#   DOCKER=podman ./…/build.sh                               # another engine
#   DOCKER="sudo docker" ./…/build.sh                        # rootful daemon from a non-root user
#   FEATURES=docker ./…/build.sh                             # trim the imbhd feature set
#   BASE=debian:trixie-slim ./…/build.sh                     # host glibc newer than bookworm's
#   IMBH_BIND=none ./…/build.sh                              # no TCP listeners at all
#
# Then:  ${DOCKER} plugin enable "${PLUGIN}"
set -eu

# The container engine. Deliberately expanded **unquoted** at every call site so a multi-word value
# ("sudo docker", "docker --context remote") splits into arguments the way the caller intends.
DOCKER=${DOCKER:-docker}
PLUGIN=${PLUGIN:-imbh/log-driver:latest}
# `docker` is the plugin API. `grpc` matters more than it looks: OTLP/gRPC on 4317 is the *default*
# transport for most OTel SDKs, so without it an app container pointed at this plugin fails until
# someone works out that it has to say `http/protobuf`. `tracing` sends imbhd's own diagnostics to
# stderr, which the Docker daemon log captures. `docker-remap` is what makes a container's JSON,
# logfmt, klog/glog or key=value output land as queryable fields instead of an opaque string --
# unlike the other three it adds a real dependency subtree (vrl; see the feature's comment in
# ../Cargo.toml), so `FEATURES=docker,grpc,tracing ./build.sh` is the smaller build. None are on by
# default (ARCHITECTURE.md §11), so a shipping build has to name them; this mirrors release.yml's
# Linux legs.
FEATURES=${FEATURES:-docker,docker-remap,grpc,tracing}
# The rootfs base. Default matches what the release publishes; raise it when this host's glibc is
# newer than the default's -- see the smoke test below, which is what tells you.
BASE=${BASE:-debian:bookworm-slim}
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "${HERE}/../../.." && pwd)
IMAGE=imbh-log-driver-rootfs

if ! command -v "${DOCKER%% *}" > /dev/null 2>&1; then
  echo "build.sh: ${DOCKER%% *} not found -- a managed plugin can only be created by a container engine." >&2
  exit 1
fi

cd "${ROOT}"

if [ ! -f THIRD-PARTY-NOTICES.txt ]; then
  echo "build.sh: THIRD-PARTY-NOTICES.txt is missing -- it must ship in the rootfs (Apache-2.0 §4(d))." >&2
  echo "  Generate it first (networked env): ./scripts/gen-notices.sh" >&2
  exit 1
fi

# The Dockerfile selects the binary with BuildKit's ${TARGETARCH}, whose values are Go-style, so
# translate uname's spelling to match.
case "$(uname -m)" in
  x86_64 | amd64) arch=amd64 ;;
  aarch64 | arm64) arch=arm64 ;;
  *)
    echo "build.sh: unsupported host architecture $(uname -m)" >&2
    exit 1
    ;;
esac

echo "==> cargo build --release (imbhd features: ${FEATURES})"
cargo build --release -p imbh-server --features "${FEATURES}"

# Under target/ so it inherits .gitignore; the context root is this directory, so the repo-root
# .dockerignore (which excludes target/) does not apply to it.
CTX="${ROOT}/target/plugin-ctx"
rm -rf "${CTX}"
mkdir -p "${CTX}/linux/${arch}"
install -m 0755 target/release/imbhd "${CTX}/linux/${arch}/imbhd"
# Apache-2.0 §4(d): the notices travel with every binary distribution, this rootfs included.
install -m 0644 LICENSE "${CTX}/LICENSE"
install -m 0644 THIRD-PARTY-NOTICES.txt "${CTX}/THIRD-PARTY-NOTICES.txt"

echo "==> building ${IMAGE} (linux/${arch}, base: ${BASE}, engine: ${DOCKER})"
# BuildKit is required, not merely preferred: ${TARGETARCH} is a BuildKit feature and silently
# expands to nothing under the legacy builder.
DOCKER_BUILDKIT=1 ${DOCKER} build -f "${HERE}/Dockerfile" \
  --build-arg "BASE=${BASE}" -t "${IMAGE}" "${CTX}"

echo "==> smoke-testing the rootfs"
# imbhd is compiled against the HOST's glibc but runs against ${BASE}'s, so a dev machine newer than
# the base produces a binary the plugin cannot start. Left to the plugin lifecycle that surfaces as a
# bare failure to enable, with the real error buried in the daemon log -- there is no
# `docker plugin logs`. So provoke it here, where it is one clear message.
#
# imbhd has no --version flag; starting it with every listener disabled and no plugin socket makes it
# initialise the database and then exit non-zero with "nothing to serve", which proves the binary
# loads, links, and can open a DB. Same assertion release.yml's binary smoke test makes.
out=$(${DOCKER} run --rm -e IMBH_LISTEN_ADDR= -e IMBH_GRPC_LISTEN_ADDR= \
  "${IMAGE}" /usr/bin/imbhd /var/lib/imbh 2>&1 || true)
printf '%s\n' "${out}"
if ! printf '%s' "${out}" | grep -qi 'nothing to serve'; then
  echo "build.sh: the staged imbhd does not run inside ${BASE} (see above)." >&2
  echo "  A 'GLIBC_... not found' line means this host's glibc ($(ldd --version 2>/dev/null | head -1))" >&2
  echo "  is newer than ${BASE}'s. Re-run against a newer base, e.g." >&2
  echo "      BASE=debian:trixie-slim $0" >&2
  echo "  The published plugin is unaffected: CI stages release binaries built on glibc 2.35." >&2
  exit 1
fi

WORK=$(mktemp -d)
trap 'rm -rf "${WORK}"' EXIT INT TERM
mkdir -p "${WORK}/rootfs"

echo "==> exporting the rootfs"
# `create` + `export` is the only way to get a flattened filesystem out of an image with the plain
# engine CLI. CI uses buildx's `--output type=tar` instead, which needs no container at all -- but
# that is a BuildKit-only feature, and this path stays engine-agnostic so DOCKER=podman keeps working.
CID=$(${DOCKER} create "${IMAGE}" true)
${DOCKER} export "${CID}" | tar -x -C "${WORK}/rootfs"
${DOCKER} rm -f "${CID}" > /dev/null
cp "${HERE}/config.json" "${WORK}/config.json"

echo "==> creating plugin ${PLUGIN}"
# Disable + remove any previous build of the same name; `plugin create` refuses to replace. It also
# refuses `content sha256:...: already exists` when a plugin under a DIFFERENT name already holds a
# byte-identical rootfs -- the plugin store is content-addressed, one plugin per digest -- so remove
# that one too if you are re-registering the same build under a second name.
${DOCKER} plugin disable -f "${PLUGIN}" > /dev/null 2>&1 || true
${DOCKER} plugin rm -f "${PLUGIN}" > /dev/null 2>&1 || true
${DOCKER} plugin create "${PLUGIN}" "${WORK}"

# Point the OTLP listeners at the bridge address *this* daemon actually uses, rather than trusting
# config.json's 172.17.0.1 default. That address is reachable from containers on every bridge network
# (`--add-host=host.docker.internal:host-gateway` resolves to it) but has no route from the LAN, so
# apps can ship traces and metrics to the plugin without the endpoint leaving the machine. A daemon
# configured with a custom `bip` uses something else entirely, which is why this is asked, not
# assumed. Settable later without a rebuild: `${DOCKER} plugin set ${PLUGIN} IMBH_LISTEN_ADDR=...`.
#
# Override with IMBH_BIND=<addr> to choose explicitly, or IMBH_BIND=none for no TCP listeners at all
# (container logs still flow -- they use the plugin's Unix socket, not the network).
BIND=${IMBH_BIND:-$(${DOCKER} network inspect bridge \
  --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}' 2>/dev/null || true)}

if [ "${BIND}" = none ]; then
  echo "==> disabling both TCP listeners (IMBH_BIND=none)"
  ${DOCKER} plugin set "${PLUGIN}" IMBH_LISTEN_ADDR= IMBH_GRPC_LISTEN_ADDR=
  ENDPOINT="(no TCP listener)"
elif [ -n "${BIND}" ]; then
  echo "==> binding OTLP to ${BIND} (this daemon's bridge gateway)"
  ${DOCKER} plugin set "${PLUGIN}" "IMBH_LISTEN_ADDR=${BIND}:4318" "IMBH_GRPC_LISTEN_ADDR=${BIND}:4317"
  ENDPOINT="http://${BIND}:4318"
else
  echo "==> WARNING: could not read this daemon's bridge gateway; leaving the config.json default"
  echo "    Set it yourself: ${DOCKER} plugin set ${PLUGIN} IMBH_LISTEN_ADDR=<addr>:4318"
  ENDPOINT="http://172.17.0.1:4318 (unverified default)"
fi

cat <<EOF

created ${PLUGIN}   OTLP endpoint: ${ENDPOINT}

  ${DOCKER} plugin enable ${PLUGIN}   # the database directory is provisioned by the daemon
  ${DOCKER} run --log-driver ${PLUGIN} --log-opt imbh-service=web nginx

  # apps reach the OTLP endpoint through the same address, on any bridge network:
  ${DOCKER} run --add-host=host.docker.internal:host-gateway \\
    -e OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4318 \\
    -e OTEL_SERVICE_NAME=web --log-driver ${PLUGIN} --log-opt imbh-service=web myapp

You do not need to build this yourself unless you are changing imbh: the release publishes a plugin
per architecture -- see docs/DOCKER_LOG_DRIVER.md "Install".
EOF
