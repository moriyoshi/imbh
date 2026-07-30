#!/usr/bin/env bash
# Third-party notice generation (QUALITY_GATE.md §3; the Rust analogue of cornus's audit-licenses).
#
# Renders THIRD-PARTY-NOTICES.txt for the binaries imbh actually distributes — `imbhd`
# (imbh-server) AND `imbh-tui` — using cargo-about + the repo-root about.toml / about.hbs.
#
# SCOPE: `--workspace --all-features`, deliberately a SUPERSET of any single build. This file ships
# inside every release archive and the container image (Apache-2.0 §4(d), README "License"), so it
# has to cover every feature combination we hand to users: the release pipeline builds with
# `grpc,tracing` (+ `docker` on Linux), and `grpc` alone pulls the whole tonic/hyper/h2/tower
# subtree. Scoping to `crates/imbh-server/Cargo.toml` with default features — as this script did
# before — attributed none of that, and nothing of `imbh-tui`'s ratatui/crossterm/rand subtree.
# Over-attributing is safe (crediting more than required); under-attributing is a licence breach, so
# the superset is intentional and must not be narrowed to "just what this build links".
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
echo "gen-notices: generating ${OUT} for imbhd + imbh-tui (workspace, all features) ..."
# -c pins the repo-root config regardless of the manifest's directory; about.toml's `targets` list
# every platform we publish for, so target-specific deps (windows-sys, core-foundation, ...) are
# covered too. --workspace + --all-features is the superset rationale documented above.
cargo about generate \
  -c about.toml \
  --manifest-path Cargo.toml \
  --workspace \
  --all-features \
  about.hbs \
  -o "${OUT}"
echo "gen-notices: wrote ${OUT}"
