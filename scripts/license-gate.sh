#!/usr/bin/env bash
# License-compatibility gate (QUALITY_GATE.md §3; ARCHITECTURE.md §11 dependency policy).
#
# Runs `cargo deny check licenses` against deny.toml (repo root), whose allow-list is mirrored by
# about.toml's `accepted` list. Fails (non-zero) if any dependency carries a license outside the
# allowlist.
#
# OFFLINE CAVEAT: the license check itself needs no network, but `cargo-deny` is not installed in
# the offline dev container. When it is absent this script prints an install hint and exits 0
# (graceful skip) so an offline gate never breaks.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo-deny >/dev/null 2>&1; then
  echo "license-gate: cargo-deny not installed — skipping license check."
  echo "  Install it (networked env): cargo install cargo-deny"
  echo "  Then re-run: ./scripts/license-gate.sh"
  exit 0
fi

echo "license-gate: cargo deny check licenses ..."
cargo deny check licenses
echo "license-gate: OK"
