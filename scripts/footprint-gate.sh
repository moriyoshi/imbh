#!/usr/bin/env bash
# Footprint gate (OVERVIEW.md §2 / ARCHITECTURE.md Appendix C, QUALITY_GATE.md §2).
#
# Measures the two failing footprint axes we can check locally — unique crate count and the
# release `imbhd` binary size — against the §2 budgets, and fails (exit 1) if either exceeds its
# HARD limit. Warns (exit 0) between target and hard limit. The third §2 axis, idle/steady RSS,
# is measured by the `examples/rss-probe` soak harness at the end of this script but is
# MEASUREMENT-ONLY: it prints numbers and WARNs, and never flips the gate to FAIL (the §2 RSS
# budgets are musl/anonymous-RSS targets, whereas the harness reads glibc VmRSS — an upper bound).
#
# Note: budgets target x86_64-musl; this measures the host (aarch64/x86_64 glibc), an
# order-of-magnitude floor, not the musl number (Appendix C caveats).
set -euo pipefail
cd "$(dirname "$0")/.."

CRATE_TARGET=275
CRATE_HARD=300
# imbhd binary budgets are the §2 musl targets; on glibc treat them as a soft ceiling.
BIN_TARGET_MB=42
BIN_HARD_MB=55

fail=0

echo "== crate count (imbh facade, normal edges) =="
crates=$(cargo tree -p imbh --edges normal --prefix none 2>/dev/null \
  | sed 's/ (\*)//; s/ v[0-9].*//' | awk 'NF' | sort -u | wc -l)
echo "  unique crates: $crates  (target <= $CRATE_TARGET, hard <= $CRATE_HARD)"
if [ "$crates" -gt "$CRATE_HARD" ]; then
  echo "  FAIL: over the hard crate limit"; fail=1
elif [ "$crates" -gt "$CRATE_TARGET" ]; then
  echo "  WARN: over target (under hard limit)"
fi

echo "== release imbhd binary size =="
bin=target/release/imbhd
# ALWAYS build, never "reuse it if the file is there". `target/release/imbhd` is one path shared by
# every feature set, and anyone who has run `cargo build --release -p imbh-server --features
# docker,docker-remap,grpc,tracing` by hand leaves the PLUGIN binary sitting at it. A skip-if-present
# check then measures that instead — silently, and against the DEFAULT-build budget: measured
# 39,997,776 B (the plugin build) where the default build is 34,916,248 B, a 5.1 MB phantom. It fails
# in the dangerous direction too, reporting a stale small binary after a real regression. Building
# unconditionally costs nothing when the tree is already current (cargo no-ops) and is the only thing
# that reconciles the feature set with the path.
echo "  building (release, fat LTO; no-op if current) ..."
cargo build --release -p imbh-server >/dev/null 2>&1
bytes=$(stat -c%s "$bin")
mib=$(awk "BEGIN{printf \"%.1f\", $bytes/1048576}")
echo "  imbhd: $bytes bytes = ${mib} MiB  (glibc floor; musl target <= ${BIN_TARGET_MB} MB, hard <= ${BIN_HARD_MB} MB)"
hard_bytes=$((BIN_HARD_MB * 1000 * 1000))
target_bytes=$((BIN_TARGET_MB * 1000 * 1000))
if [ "$bytes" -gt "$hard_bytes" ]; then
  echo "  FAIL: over the hard binary limit"; fail=1
elif [ "$bytes" -gt "$target_bytes" ]; then
  echo "  WARN: over target (under hard limit) — expected on glibc; confirm on musl"
fi

echo "== shipped plugin build (informational — never fails the gate) =="
# The two axes above measure the LIBRARY graph (`cargo tree -p imbh`) and a DEFAULT-feature imbhd.
# The published Docker log-driver plugin is neither: it is imbh-server with
# `docker,docker-remap,grpc,tracing`, and `docker-remap` pulls vrl — the one feature in the workspace
# that adds crates on purpose. Nothing else in this script can see that, so print it here rather than
# let it surface at release time. Informational ONLY: the §2 budgets are written against the default
# build, and turning the plugin's size into a hard gate would be a new policy, not a measurement.
# Skip with PLUGIN_PROBE=0 (it is a fat-LTO build, so it is not cheap).
if [ "${PLUGIN_PROBE:-1}" = "0" ]; then
  echo "  skipped (PLUGIN_PROBE=0)"
else
  count_crates() {
    cargo tree -p imbh-server --edges normal --prefix none --features "$1" 2>/dev/null \
      | sed 's/ (\*)//; s/ v[0-9].*//' | awk 'NF' | sort -u | wc -l
  }
  base_crates=$(count_crates docker,grpc,tracing)
  remap_crates=$(count_crates docker,docker-remap,grpc,tracing)
  echo "  imbh-server crates: $base_crates (docker,grpc,tracing) -> $remap_crates (+docker-remap) = +$((remap_crates - base_crates))"
  echo "  building plugin feature set (release, fat LTO) ..."
  # Its OWN target dir, deliberately: the gated binary axis above reuses `target/release/imbhd` when
  # it already exists, so building a different feature set into that path would silently make the
  # next gate run measure the plugin build as if it were the default one. Costs disk and a cold
  # build; buys a measurement that cannot corrupt the thing it sits next to.
  probe_dir=target/footprint-plugin-probe
  if cargo build --release -p imbh-server --features docker,docker-remap,grpc,tracing \
       --target-dir "$probe_dir" >/dev/null 2>&1; then
    pbytes=$(stat -c%s "$probe_dir/release/imbhd")
    pmib=$(awk "BEGIN{printf \"%.1f\", $pbytes/1048576}")
    echo "  imbhd (plugin feature set): $pbytes bytes = ${pmib} MiB"
  else
    echo "  WARN: the plugin feature set did not build"
  fi
fi

echo "== engine deps present? =="
# `search` and `query` are both ON by default (crates/imbh/Cargo.toml), so both engines MUST be in
# the default graph. Their absence here is never a deliberate footprint trim — the deliberate trim is
# the search-off lever measured below, which uses an explicit `--no-default-features`. Absence here
# means a feature-flag edit silently dropped a subsystem, which is a regression, so these FAIL the
# gate rather than printing a note nobody reads (matching the search-lever guard below).
#
# `grep -q <<<"$tree"`, NOT `printf ... | grep -q`. `grep -q` exits the instant it matches, which
# under this script's `set -o pipefail` can leave the upstream writer with a broken pipe and make the
# pipeline report the writer's SIGPIPE (141) even though grep matched — a false "not present" on a
# perfectly healthy graph. The window only opens once the writer outsizes the pipe buffer (64 KiB;
# the tree is ~55 KiB today, so the current form does not actually race — but it is one dependency
# away from doing so, and the failure it would produce reads as "the query engine vanished"). A
# here-string has no upstream process and cannot race at any size. Keep every `grep -q` fed by one.
#
# Guard the capture first. `2>/dev/null` swallows cargo's own errors, so a `cargo tree` that fails —
# a lock held by a concurrent cargo, a manifest error — yields an EMPTY `$tree`, and every check
# below would read that as "the engine was dropped" and fail the gate. Distinguish "cargo could not
# tell us" from "the answer is no": the former is an infrastructure failure, not a footprint result.
tree_err=''
if ! tree=$(cargo tree -p imbh --edges normal 2>&1) || [ -z "$tree" ]; then
  tree_err=${tree:-(no output)}
  tree=''
fi
if [ -z "$tree" ]; then
  echo "  FAIL: \`cargo tree -p imbh\` produced no graph, so the engine checks could not run:"
  printf '%s\n' "$tree_err" | head -5 | sed 's/^/    /'
  fail=1
elif ! grep -q 'tantivy v' <<<"$tree"; then
  echo "  FAIL: no tantivy in the default graph — the search engine was silently dropped"; fail=1
else
  echo "  tantivy: yes"
fi
# Match the whole DataFusion crate FAMILY, not just the bare `datafusion` facade. DataFusion keeps
# splitting itself into sub-crates (datafusion-core / -session / -physical-plan / ...), so a pattern
# pinned to `datafusion v` would report a perfectly healthy tree as broken the day imbh-query depends
# on a split crate instead of the facade. Every datafusion* crate reaches this graph only through
# imbh-query, so the family is present iff `query` is: verified empirically — the default tree has
# 31 datafusion crates (189 lines), `cargo tree -p imbh --no-default-features` has zero.
if [ -z "$tree" ]; then
  : # already reported above; do not repeat the same infrastructure failure twice
elif ! grep -qE 'datafusion(-[a-z-]+)? v' <<<"$tree"; then
  echo "  FAIL: no datafusion crate in the default graph — the query engine was silently dropped"; fail=1
else
  echo "  datafusion: yes"
fi

echo "== search-off footprint lever (§11) =="
# TWO different knobs live here and they used to get conflated (QUALITY_GATE.md §2 quoted one knob's
# number against the other's name):
#   `--no-default-features --features ingest,query` — search OFF and nothing else; the delta from the
#       default graph is the tantivy subtree, i.e. what "turning search off costs".
#   `--no-default-features`                          — ingest AND query AND search off, so it also
#       takes the OTLP-decode and the whole DataFusion subtree. Much bigger delta, nothing to do with
#       search. This is the config the compile guard below builds.
# Print both, labelled, so the big delta can never be misread as the cost of tantivy again.
#
# Each tree is captured ONCE into a variable and then both counted and grepped from a here-string. The
# old form piped `cargo tree` straight into `grep -q`, which has the same SIGPIPE-under-pipefail race
# described above — and here it fails the DANGEROUS way round: a broken pipe makes the `if` read as
# "tantivy not found", i.e. the guard reports the lever healthy at exactly the moment tantivy leaked
# back in and grep matched early. `--prefix none` keeps the `name vX.Y.Z` text both uses need.
count_tree() {
  printf '%s\n' "$1" | sed 's/ (\*)//; s/ v[0-9].*//' | awk 'NF' | sort -u | wc -l
}
searchoff_tree=$(cargo tree -p imbh --no-default-features --features ingest,query \
  --edges normal --prefix none 2>/dev/null)
off_tree=$(cargo tree -p imbh --no-default-features --edges normal --prefix none 2>/dev/null)
searchoff=$(count_tree "$searchoff_tree")
off=$(count_tree "$off_tree")
echo "  default (ingest,query,search):  $crates"
echo "  search off only (ingest,query): $searchoff  (-$((crates - searchoff)) = the tantivy subtree)"
echo "  all default features off:       $off  (-$((crates - off)); also drops OTLP decode + DataFusion)"
# The §11 `search` knob must (a) still compile and (b) actually drop the tantivy subtree. Guard both
# so the lever can't silently break (e.g. an ungated imbh_index reference re-links tantivy). Check the
# precise lever (`ingest,query`) as well as the bare build: tantivy can leak back into either.
if grep -q 'tantivy v' <<<"$searchoff_tree"; then
  echo "  FAIL: tantivy still present with search off (ingest,query) — the feature lever is broken"; fail=1
elif grep -q 'tantivy v' <<<"$off_tree"; then
  echo "  FAIL: tantivy still present with all default features off — the feature lever is broken"; fail=1
else
  echo "  tantivy dropped: yes"
fi
echo "  compiling search-off config ..."
if cargo build -p imbh --no-default-features >/dev/null 2>&1; then
  echo "  search-off build: OK"
else
  echo "  FAIL: imbh --no-default-features does not compile"; fail=1
fi

echo "== idle/steady RSS soak (examples/rss-probe) =="
# The third §2 footprint axis, previously *(unmeasured, M1)*. MEASUREMENT-ONLY: never touches
# `fail`, so RSS can't flip the gate. The §2 budgets target x86_64-musl anonymous RSS; the harness
# reads glibc VmRSS from /proc/self/status (file-backed mmap pages included — an upper bound; see
# the rss-probe crate doc-comment), so we print alongside the §2 targets and WARN if over, no more.
# The gate uses a small, fast record count (a smoke number, not a sustained 10k-rec/s soak); run
# `cargo run --release -p rss-probe -- <big>` for a real soak. Skip with RSS_PROBE=0; override the
# count with RSS_PROBE_RECORDS.
RSS_IDLE_TARGET_MB=40;    RSS_IDLE_HARD_MB=64
RSS_STEADY_TARGET_MB=200; RSS_STEADY_HARD_MB=320
if [ "${RSS_PROBE:-1}" = "0" ]; then
  echo "  skipped (RSS_PROBE=0)"
else
  recs="${RSS_PROBE_RECORDS:-20000}"
  echo "  running rss-probe (debug, ${recs} records) ..."
  rssline=$(cargo run -q -p rss-probe -- "$recs" 2>/dev/null | grep '^RSS_PROBE ' || true)
  if [ -z "$rssline" ]; then
    echo "  WARN: rss-probe produced no summary line (build blocked, or non-Linux host) — RSS not measured"
  else
    idle_kib=$(printf '%s\n' "$rssline" | sed -n 's/.*idle_kib=\([0-9]*\).*/\1/p')
    steady_kib=$(printf '%s\n' "$rssline" | sed -n 's/.*steady_kib=\([0-9]*\).*/\1/p')
    idle_mb=$(awk "BEGIN{printf \"%.1f\", ${idle_kib:-0}*1024/1000000}")
    steady_mb=$(awk "BEGIN{printf \"%.1f\", ${steady_kib:-0}*1024/1000000}")
    echo "  idle RSS:   ${idle_kib} kiB = ${idle_mb} MB  (glibc VmRSS; musl target <= ${RSS_IDLE_TARGET_MB} MB, hard <= ${RSS_IDLE_HARD_MB} MB)"
    if awk "BEGIN{exit !(${idle_kib:-0}*1024/1000000 > $RSS_IDLE_HARD_MB)}"; then
      echo "    WARN: over idle hard limit (measurement only; gate not failed)"
    elif awk "BEGIN{exit !(${idle_kib:-0}*1024/1000000 > $RSS_IDLE_TARGET_MB)}"; then
      echo "    WARN: over idle target (measurement only; confirm on musl release)"
    fi
    echo "  steady RSS: ${steady_kib} kiB = ${steady_mb} MB  (glibc VmRSS; musl target <= ${RSS_STEADY_TARGET_MB} MB, hard <= ${RSS_STEADY_HARD_MB} MB)"
    if awk "BEGIN{exit !(${steady_kib:-0}*1024/1000000 > $RSS_STEADY_HARD_MB)}"; then
      echo "    WARN: over steady hard limit (measurement only; gate not failed)"
    elif awk "BEGIN{exit !(${steady_kib:-0}*1024/1000000 > $RSS_STEADY_TARGET_MB)}"; then
      echo "    WARN: over steady target (measurement only; confirm on musl release)"
    fi
  fi
fi

[ "$fail" -eq 0 ] && echo "FOOTPRINT GATE: OK" || echo "FOOTPRINT GATE: FAIL"
exit "$fail"
