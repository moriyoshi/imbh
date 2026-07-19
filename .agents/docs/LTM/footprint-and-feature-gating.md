# Footprint and Feature Gating

## Summary

Footprint is a first-class imbh requirement measured on three axes: unique crate count in the dependency graph, release binary size, and runtime RSS. The v0.1 footprint exit criterion is MET on the crate-count and binary-size axes (275 crates / 31.2 MiB `imbhd`, both within budget), enforced by a repeatable footprint gate. The largest footprint levers are the `search` feature (Tantivy on/off, -59 crates) and the M6c producer/consumer split (`ingest`/`query` features, producer -64%), while OTLP-proto vendoring was measured and dismissed as a no-op on binary size.

## Key Facts

- Prefer `default-features = false` with a minimal feature set for every heavy dependency (as the M0 probe does for DataFusion, Tantivy, and opentelemetry-proto).
- Budgets (`OVERVIEW.md` §2 / `ARCHITECTURE.md` §11, Appendix C): unique crates ≤ 275 target / ≤ 300 hard; `imbhd` binary ≤ 42 MB musl target / ≤ 55 MB hard.
- The v0.1 footprint exit criterion is MET on the binary/crate-count axes; idle/steady/peak RSS still needs a soak harness (tracked); the musl binary number is a separate measurement task pending that target's setup.
- DataFusion is the dominant footprint driver; `search` (Tantivy) is the single largest severable lever; the producer/consumer split (dropping the query engine) is a larger lever still.
- Dependency-graph presence != binary weight — LTO/DCE make the shipped-binary measurement the one that matters, and `cargo bloat` is the right probe.
- `opentelemetry_sdk` is forced into the tree by `opentelemetry-proto` 0.32's feature design, but release DCE strips it to 0 bytes in the shipped binary.

## Details

### M0 footprint probe and budget baseline

Versions pinned per Appendix A: datafusion 54, opentelemetry-proto 0.32, prost 0.14, tokio 1, tempfile 3; edition 2024, rustc 1.96 (host aarch64-glibc). Engine features were trimmed exactly to the Appendix C set. The `release` profile carries the §11 shipping settings (`opt-level="s"` / fat LTO / `codegen-units=1` / strip / `panic="abort"`).

At M0 the `imbh` facade graph was **216** unique normal-edge crates (≤ 275 budget). Tantivy was not wired yet, so this was a lower bound; M1 adds its subtree. The M0 engine-only probe measured a **31.9 MiB** binary-size floor for the trimmed engine set.

### Footprint gate + cargo-deny + release measurement (v0.1 footprint MET)

`scripts/footprint-gate.sh` measures the two locally-checkable footprint axes (unique crate count via `cargo tree -p imbh`, release `imbhd` binary size) against the §2 budgets and exits non-zero over a hard limit — wiring QUALITY_GATE §2 as one repeatable command.

`deny.toml` is the cargo-deny config: permissive-license allowlist (Apache-2.0/MIT/BSD/ISC/…), `duplicate-version = warn`, `wildcards = deny`, and an explicit **no-openssl** ban (§11). `cargo-deny` is installed in the dev container and the check runs in CI (`ci.yml` `licenses` job on default-branch pushes, and `release.yml` on `v*` tags).

Measured (aarch64-glibc, release-small profile: `opt-level="s"` + fat LTO + strip + panic=abort):

| Metric | Value | §2 budget | Verdict |
|---|---|---|---|
| Unique crates (imbh facade, normal edges) | **275** | ≤ 275 target / ≤ 300 hard | at target |
| `imbhd` release binary | **31.2 MiB** | ≤ 42 MB musl target / ≤ 55 MB hard | well under (glibc floor) |

The full `imbhd` (whole library + HTTP server) at **31.2 MiB** is *below* the M0 probe's 31.9 MiB engine-only floor — release LTO + strip claws back more than the added business logic costs. This confirms the Appendix C GO decision on the real, complete binary.

### opentelemetry_sdk is forced into the tree

`opentelemetry` + `opentelemetry_sdk` sit in the shipped `imbh` facade normal-deps tree even though imbh only decodes raw OTLP wire bytes (prost messages) and never uses the SDK. Root cause (not an imbh misconfiguration): `opentelemetry-proto` 0.32 cfg-gates the *message modules* (`tonic::logs/metrics/trace::v1::*`) behind the same `logs`/`metrics`/`trace` cargo features that also pull `opentelemetry_sdk/{logs,metrics,trace}` (for the SDK→proto `From` transforms). There is no "messages-only, no SDK" feature. Confirmed in `opentelemetry-proto-0.32.0/src/proto.rs` (`#[cfg(feature = "logs")]` on the module) and its `[features]` (`logs = ["opentelemetry/logs", "opentelemetry_sdk/logs"]`).

The SDK's transitive subtree is ~29 crates, but almost all of them (futures-*, rand, thiserror, syn/quote/proc-macro2, memchr, zerocopy, percent-encoding…) are already pulled by the datafusion/tantivy/tonic graphs, so the SDK's *unique* contribution is small — chiefly the 3 `opentelemetry*` crates. Total holds at 275 crates / 31.2 MiB. Not a regression. The only real lever would be to vendor/generate just the OTLP proto message types with prost and drop `opentelemetry-proto` (→ drop the SDK) — a large, risky change (build-time codegen or committed generated code) for a small unique-crate win when already under budget. A side effect: because the SDK **and** opentelemetry-proto's SDK→tonic transforms are already compiled into the tree, the optional `imbh-otel-exporter` crate adds ~0 new footprint (it converts SDK batches → OTLP tonic via existing transforms → encode → `db.ingest_otlp_*`).

### OTLP-proto vendoring dismissed (measured, no binary-size win)

Tested against the real binary instead of the crate graph. `cargo bloat --release -p imbh-server --bin imbhd --crates` on the release `imbhd`: `opentelemetry_sdk` = **0 bytes** (does not appear — its `From<sdk-type>` proto transforms are unreachable from imbh-otlp's message-only usage, so release dead-code elimination strips the whole crate), `opentelemetry` = **244 bytes**, `opentelemetry-proto` not listed. Removable text ≈ 244 B / 42.2 MiB = 0.0006 %. The prost message structs imbh actually decodes would remain after vendoring, so vendoring buys ~nothing on the binary axis (only a unique-crate / compile-time trim, at codegen risk). Dismissed and marked done.

### `search` feature — the biggest single feature-matrix lever

`search` (Tantivy on/off) is the single largest footprint lever and is cleanly severable because `matches()` already works as a row-by-row scan without the index — unlike `sql`, which is not severable until the typed APIs stop routing through sqlparser. imbh-index usage was shallow: 2 real call sites in storage (`build_logs_index` at seal + compact) and 1 in query (`search_body` in the RowSelection bridge), so the cut is bounded.

Landed end-to-end across 3 crates + workspace:
- **imbh-storage**: `imbh-index` → `optional`; `[features] default=["search"] search=["dep:imbh-index"]`. Confined `imbh_index` to one `build_logs_sidecar` helper pair (a real fn under `search`, a no-op `Ok(())` without) so both call sites and their locals stay live — no cfg at call sites. Gated the `.tidx` test.
- **imbh-query**: same feature shape. Two `row_selection_for` variants (real pruning under `search`; `Ok(None)` = plain scan without). Gated the search-only `SELECTIVITY_THRESHOLD`, the `RowSelector` import, `row_selection_from_sorted`, and `mod tests`.
- **imbh facade**: `search` feature (default on) forwards to `imbh-storage/search` + `imbh-query/search` (+ `dep:imbh-index`). The facade never references `imbh_index` in code (that dep was dead); it reads `index_path` from `.tidx` file existence (`idx.is_dir()`), so with search off no sidecar is written → `index_path` is always `None` → plain scan, with zero facade code changes.
- **Workspace fix (subtle):** `imbh --no-default-features` initially still pulled tantivy, because the facade inherited imbh-storage/imbh-query with *their* `default=["search"]`. Cargo forbids overriding `default-features` on a `workspace = true` inherited dep, so the fix went in the workspace table: `imbh-storage` / `imbh-query` are declared `default-features = false` there. The facade re-enables search via its own feature; standalone `cargo test -p imbh-storage` still uses the crate's own `default=["search"]`.

Footprint result: `cargo tree -p imbh` = 275 crates (search on) vs `cargo tree -p imbh --no-default-features` = **216 crates — 59 dropped**, the whole tantivy subtree (16 tantivy refs → 0). The search-off path's behavior-identity is proven directly — the entire 41-test facade suite passes under `cargo test -p imbh --no-default-features`, i.e. `matches()` returns identical results via scan with no index. Minor accepted cost: in a search-off compact, `logs_batch_to_index_rows(&sorted)` is still computed then discarded by the no-op sidecar.

Follow-ups:
- **Regression guard (done).** `scripts/footprint-gate.sh` gained a "search-off footprint lever" section: it runs `cargo tree -p imbh --no-default-features` and **fails** if tantivy is still linked (catches an ungated `imbh_index` reference silently re-adding the subtree), and compiles the search-off config so a broken `--no-default-features` build fails the gate too. Gate prints: `search-off unique crates: 216 (search-on: 275) … tantivy dropped: yes (-59 crates) … search-off build: OK … FOOTPRINT GATE: OK`.
- **Knob-forwarding to imbhd (deferred, low-value).** Letting imbh-server / imbh-otel-exporter build search-free would require `imbh` itself to be `default-features = false` in the workspace table, cascading the search re-enable to *every* imbh consumer (the exporter + all examples) — real churn and a wide feature-unification blast radius. imbhd is a *reference wiring, not the product*; the library-level knob (`imbh --no-default-features`, already working) is what a footprint-conscious embedder uses. Revisit only if a host actually needs a search-free imbhd.

### Milestone-completeness footprint checkpoints

A full end-to-end verification (release-small, aarch64-glibc) at the M0–M6 completion checkpoint recorded:
- Unique crates: **275** (target ≤ 275, hard ≤ 300) — unchanged across the session; every feature added reused existing deps. Only the optional `imbh-otel-exporter` added a crate, and it is off the core `imbh`/`imbhd` graph.
- `imbhd` binary: **32.0 MiB** (was 31.2 MiB earlier) — +0.8 MiB across the session (span search, all 5 metric families, durability code); well within the 42 MB musl target / 55 MB hard limit.
- `search`-off lever intact: `imbh --no-default-features` = 216 crates, tantivy dropped (-59), compiles.
- `FOOTPRINT GATE: OK`.

Beyond-core footprint follow-ups tracked at this checkpoint: the per-signal `logs`/`traces`/`metrics` feature matrix (a binary-size lever, no crate-count win), `cargo about` notice generation (needs network), and the allocator feature.

### M6c producer/consumer feature gating

M6c was reframed from per-signal gating (implemented then reverted) to a producer/consumer split — the bigger footprint lever. A producer ingests only; a consumer queries only. The win: a producer needs no query engine.

- **Phase 0 — decoupled imbh-storage from the `datafusion` crate.** Storage used DataFusion only through `datafusion::{arrow,parquet}` re-exports (no execution/logical_expr/physical_plan), so it now depends on `arrow` + `parquet` directly (workspace-pinned 58.3.0 = datafusion 54's exact arrow, keeping one arrow/parquet in the tree). Added a `parquet` workspace dep with only the codecs storage maps (`arrow`, `zstd`, `lz4`). Verified: a producer-only probe crate round-tripped lz4/zstd with no datafusion in its tree; single-arrow ABI rule intact.
- **Phases 1-2 — facade `ingest`/`query` features.** `imbh-otlp` gated by `ingest`; `imbh-query` + `datafusion` gated by `query`; `search`⇒`query`, `proto`⇒`query`; `default = ["ingest","query","search"]`. arrow/parquet re-sourced directly in the facade (`datafusion::arrow`→`arrow`, re-export the crates) so shared/ingest paths keep Arrow types without DataFusion. Every `Db`/`BlockingDb`/`Query` method + free fn was classified query / ingest / shared and `#[cfg]`-gated; `open`'s async-ingest-worker spawn and `stats`'s ingest gauges gated in-place (statements, not whole methods); `Db` reader-cache fields gated `query`, `ingest`/`ingest_handle` fields gated `ingest`.

Measured footprint (unique crates, `cargo tree -e no-dev -p imbh`): default 287 → **producer 104 (-64%)** (drops the entire DataFusion + sqlparser + tantivy subtree) / consumer 221 / storage-only 80. Reduced builds compile + clippy `-D warnings` clean; full workspace gate green. Verified: producer tree has 0 datafusion/sqlparser/imbh-query; consumer tree has 0 imbh-otlp.

**Design consequence:** a *pure* consumer (`--features query` without `ingest`) reads **sealed segments only** — no WAL-tail replay. Root cause: WAL frames store raw OTLP bytes (`wal_append_assign(SIGNAL_*, raw, …)`) and `replay_record`'s body is imbh-otlp decode, so replay cannot exist without the OTLP decoder. Gating replay behind `ingest` is the only design that lets a consumer drop imbh-otlp. A host wanting near-real-time cross-process reads keeps `ingest` on (the default has both). A future alternative — store decoded rows in the WAL so replay needs no decoder — would let a pure consumer tail the WAL, but that is a storage-format change, out of scope.

Remaining M6c: Phase 3 mimalloc opt-in (imbhd binary only); Phase 4 feature-matrix CI (build+clippy the reduced configs; full tests stay in the default `--workspace` job); refresh ARCHITECTURE.md §11 / OVERVIEW.md §2 with the axis + numbers.

## Files

- `scripts/footprint-gate.sh` — measures unique crate count (`cargo tree -p imbh`) and release `imbhd` binary size against §2 budgets; includes the search-off lever regression guard; exits non-zero over a hard limit.
- `deny.toml` — cargo-deny config: permissive-license allowlist, `duplicate-version = warn`, `wildcards = deny`, no-openssl ban.
- `.agents/docs/QUALITY_GATE.md` — §2 points at the script and records measured numbers; §3 references `deny.toml`.
- `.agents/docs/OVERVIEW.md` §2 / `.agents/docs/ARCHITECTURE.md` §11, Appendix C — footprint budgets and the M0 probe.
- Workspace `Cargo.toml` — `imbh-storage` / `imbh-query` declared `default-features = false` (search lever); `arrow` + `parquet` (58.3.0) workspace deps (producer/consumer split).
- `opentelemetry-proto-0.32.0/src/proto.rs` — the `#[cfg(feature = "logs")]` module gating that forces `opentelemetry_sdk` into the tree.

## Test Coverage — the footprint gate

The footprint gate (`scripts/footprint-gate.sh`) is the repeatable enforcement:
- Unique crate count via `cargo tree -p imbh` vs ≤ 275 target / ≤ 300 hard.
- Release `imbhd` binary size vs ≤ 42 MB musl target / ≤ 55 MB hard.
- Search-off lever: runs `cargo tree -p imbh --no-default-features`, fails if tantivy is still linked (would mean an ungated `imbh_index` reference silently re-added the subtree), and compiles the search-off config so a broken `--no-default-features` build fails the gate.
- `cargo bloat --release -p imbh-server --bin imbhd --crates` is the right probe when a crate's *binary* (not graph) weight is in question, because LTO/DCE decouple the two.
- Feature-matrix CI (M6c Phase 4, pending) must build + clippy the reduced configs explicitly — feature unification means `cargo build --workspace` (all features unioned) compiles even when a crate's own feature set is insufficient, so only the reduced-config builds are real proof.

## Pitfalls — heavy subtrees, forced deps

- **DataFusion is the dominant footprint driver.** Do not add a heavy dependency subtree without checking it against `OVERVIEW.md` §2 and `ARCHITECTURE.md` §11 / Appendix C; prefer trimming default features (`default-features = false`) over accepting a large subtree.
- **`opentelemetry_sdk` is forced in by `opentelemetry-proto` 0.32** — there is no "messages-only, no SDK" feature; enabling `logs`/`metrics`/`trace` to get the message types drags in the SDK. Its unique contribution is small (~3 `opentelemetry*` crates) and release DCE strips it to 0 bytes, so it is not worth fighting.
- **Dependency-graph presence != binary weight.** LTO/DCE strip unreachable crates from the shipped binary (e.g. `opentelemetry_sdk` = 0 bytes despite being in the graph). Measure the binary with `cargo bloat`, not the graph, before pursuing a "drop the subtree" optimization. OTLP-proto vendoring was dismissed on exactly this basis (~244 B / 0.0006 % removable). The producer/consumer split, by contrast, drops *whole crates* (DataFusion) and is a genuine graph *and* binary win.
- **Cargo forbids overriding `default-features` on a `workspace = true` inherited dep.** To make an optional-engine feature (e.g. `search`) actually severable at the facade, declare the intermediate crates `default-features = false` in the workspace table and re-enable via the facade's own feature.
- **A pure consumer (`query` without `ingest`) cannot replay the WAL tail** — it reads sealed segments only, because WAL frames store raw OTLP bytes and replay is imbh-otlp decode. Keep `ingest` on for near-real-time cross-process reads.
- **Search-off compact still computes `logs_batch_to_index_rows(&sorted)`** then discards it via the no-op sidecar — a documented minor cost, not worth extra cfg.
