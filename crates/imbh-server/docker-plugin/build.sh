#!/bin/sh
# Build and register the imbh logging-driver plugin (docs/DOCKER_LOG_DRIVER.md).
#
# A managed plugin is a rootfs directory plus a config.json, handed to `plugin create`. There is no
# Dockerfile-to-plugin shortcut, so this builds an image, exports its filesystem, and pairs the two.
#
#   ./crates/imbh-server/docker-plugin/build.sh              # build + create
#   PLUGIN=me/imbh:dev ./…/build.sh                          # under another name
#   DOCKER=podman ./…/build.sh                               # another engine
#   DOCKER="sudo docker" ./…/build.sh                        # rootful daemon from a non-root user
#   IMBH_BIND=none ./…/build.sh                              # no TCP listeners at all
#
# Then:  ${DOCKER} plugin enable "${PLUGIN}"
set -eu

# The container engine. Deliberately expanded **unquoted** at every call site so a multi-word value
# ("sudo docker", "docker --context remote") splits into arguments the way the caller intends.
DOCKER=${DOCKER:-docker}
PLUGIN=${PLUGIN:-imbh/log-driver:latest}
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "${HERE}/../../.." && pwd)
IMAGE=imbh-log-driver-rootfs

echo "==> building ${IMAGE} from ${ROOT} (engine: ${DOCKER})"
${DOCKER} build -f "${HERE}/Dockerfile" -t "${IMAGE}" "${ROOT}"

WORK=$(mktemp -d)
trap 'rm -rf "${WORK}"' EXIT INT TERM
mkdir -p "${WORK}/rootfs"

echo "==> exporting the rootfs"
# `create` + `export` is the only way to get a flattened filesystem out of an image.
CID=$(${DOCKER} create "${IMAGE}" true)
${DOCKER} export "${CID}" | tar -x -C "${WORK}/rootfs"
${DOCKER} rm -f "${CID}" >/dev/null
cp "${HERE}/config.json" "${WORK}/config.json"

echo "==> creating plugin ${PLUGIN}"
# Disable + remove any previous build of the same name; `plugin create` refuses to replace.
${DOCKER} plugin disable -f "${PLUGIN}" >/dev/null 2>&1 || true
${DOCKER} plugin rm -f "${PLUGIN}" >/dev/null 2>&1 || true
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

  ${DOCKER} plugin set    ${PLUGIN} data.source=/var/lib/imbh   # where the database lives on the host
  ${DOCKER} plugin enable ${PLUGIN}
  ${DOCKER} run --log-driver ${PLUGIN} --log-opt imbh-service=web nginx

  # apps reach the OTLP endpoint through the same address, on any bridge network:
  ${DOCKER} run --add-host=host.docker.internal:host-gateway \\
    -e OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4318 \\
    -e OTEL_SERVICE_NAME=web --log-driver ${PLUGIN} --log-opt imbh-service=web myapp
EOF
