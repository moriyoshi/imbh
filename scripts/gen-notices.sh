#!/usr/bin/env bash
# Third-party notice generation (QUALITY_GATE.md §3; the Rust analogue of cornus's audit-licenses).
#
# Renders THIRD-PARTY-NOTICES.txt for the shipped imbhd (imbh-server) binary graph using
# cargo-about + the repo-root about.toml / about.hbs.
#
# OFFLINE CAVEAT: cargo-about resolves license text from the crates.io index (network), so the
# actual generation must run in a networked env. When cargo-about is absent this script prints an
# install hint and exits 0 (graceful skip) so an offline gate never breaks.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo-about >/dev/null 2>&1; then
  echo "gen-notices: cargo-about not installed — skipping notice generation."
  echo "  Install it (networked env): cargo install cargo-about"
  echo "  Then re-run: ./scripts/gen-notices.sh"
  exit 0
fi

OUT="THIRD-PARTY-NOTICES.txt"
echo "gen-notices: generating $OUT for imbh-server (imbhd) ..."
# --manifest-path scopes the crate graph to the shipped imbhd binary and its deps.
# -c pins the repo-root config regardless of the manifest's directory.
cargo about generate \
  -c about.toml \
  --manifest-path crates/imbh-server/Cargo.toml \
  about.hbs \
  -o "$OUT"
echo "gen-notices: wrote $OUT"
