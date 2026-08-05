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
if [ ! -f "$bin" ]; then
  echo "  building (release, fat LTO) ..."
  cargo build --release -p imbh-server >/dev/null 2>&1
fi
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
tree=$(cargo tree -p imbh --edges normal 2>/dev/null)
printf '%s\n' "$tree" | grep -q 'tantivy v' && echo "  tantivy: yes" || echo "  tantivy: NO"
printf '%s\n' "$tree" | grep -q 'datafusion v' && echo "  datafusion: yes" || echo "  datafusion: NO"

echo "== search-off footprint lever (imbh --no-default-features) =="
# The §11 `search` knob must (a) still compile and (b) actually drop the tantivy subtree. Guard both
# so the lever can't silently break (e.g. an ungated imbh_index reference re-links tantivy).
off=$(cargo tree -p imbh --no-default-features --edges normal --prefix none 2>/dev/null \
  | sed 's/ (\*)//; s/ v[0-9].*//' | awk 'NF' | sort -u | wc -l)
echo "  search-off unique crates: $off  (search-on: $crates)"
if cargo tree -p imbh --no-default-features --edges normal 2>/dev/null | grep -q 'tantivy v'; then
  echo "  FAIL: tantivy still present with search off — the feature lever is broken"; fail=1
else
  echo "  tantivy dropped: yes (-$((crates - off)) crates)"
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
