# Traces and the Typed Error Model

## Summary

imbh's traces pillar exposes a typed `traces()` query API over the `spans` table: assemble a `Trace` by id, `search(TraceQuery)` for candidate traces, compute span RED metrics (`span_metrics`), and evaluate TraceQL against streamed single traces. Trace-query correctness rests on a candidate-selection boundary fix (trace start must be computed over ALL spans of a matched trace, not just the filtered ones) and on pushdown soundness (a predicate must be both necessary and a superset the storage matcher returns). imbh-core carries a typed nested `Error` model whose classifiers drive `imbh-server`'s 4xx/5xx split.

## Key Facts

- `db.traces().get(trace_id)` assembles a `Trace`; `traces().search(TraceQuery)` returns `Vec<TraceSummary>`; `traces().span_metrics(SpanMetricsQuery)` returns RED metrics; all operate over the single `spans` table (no new storage).
- Trace ids/span ids are filtered and rendered via a `hex(binary) -> Utf8` UDF (`ScalarUDFImpl` with `Signature::any`); `hex(trace_id) = '<hex>'` is the portable, coercion-safe id filter.
- `TraceQuery::search` selects matched traces with a parameterized semi-join `hex(trace_id) IN (SELECT hex(trace_id) FROM spans WHERE <predicate>)` and aggregates `min/max(start_time)` over ALL spans of matched traces — this is the boundary-correct shape.
- TraceQL evaluation is streaming and per-trace independent (`TraceSource` / `fetch_trace` — one `TracePack` per trace), with sound candidate pushdown via `candidate_filters(&SpansetExpr) -> Vec<SpanCandidateFilter>`.
- The typed nested `Error` (all in `crates/imbh-core/src/error.rs`) is struct-per-category with a boxed `source`; classifiers `is_backpressure`/`is_not_found`/`is_user_error` are exact `matches!` on typed leaves; zero new dependencies, crate count held at 275.
- `json_get_str` returns NULL for numeric JSON scalars; `json_get_num(json,key)->Float64` was added so numeric attribute matchers (`attr_gt`/`ge`/`lt`/`le`) work on genuine `IntValue`/`DoubleValue` attributes.
- Pushdown rule: necessity is not sufficiency — the storage matcher must return a superset of what the evaluator matches.

## Details

### Typed Traces API

`db.traces().get(trace_id) -> Trace` assembles the span tree: the root is the parent-less span, root service/name are taken from it, `duration = max(start+dur) - min(start)`, and spans are ordered. `TracesApi::search(TraceQuery) -> Vec<TraceSummary>` is a two-phase, scan-based query: rank candidate trace ids by recency, then fetch all their spans and summarize in Rust. `TraceQuery` filters on service / name / min+max duration / status / kind / attr_eq / range / limit. DTOs are `Span` / `Trace` / `TraceSummary`, in the `imbh` facade `traces.rs`.

Supporting primitives: a `hex(binary) -> Utf8` UDF in `imbh-query` (the first UDF written with the `ScalarUDFImpl` trait rather than `create_udf`, because `create_udf` cannot express the "any binary type" signature). DF54 `ScalarUDFImpl` requires `Eq + Hash` (`DynEq`/`DynHash`) and has no `as_any` — derive `PartialEq, Eq, Hash` on the UDF struct (its `Signature` field implements them). `imbh-core` gained `TraceId::from_hex` / `SpanId::from_hex` (const-generic hex decode). Rendering ids to hex and comparing strings is used because comparing a `FixedSizeBinary` column to a `X'...'` binary literal is coercion-fragile. The `hex` UDF exists because the `encode`/`decode` package is disabled for footprint (see ARCHITECTURE.md §9.1).

The traces pillar exit criteria (ingest -> assembled span tree out, p99-by-route expressible in SQL) are met. Per-segment bloom filters and pushing summary aggregation into SQL are perf follow-ups.

### Span RED metrics (`traces().span_metrics`)

`traces().span_metrics(SpanMetricsQuery) -> SpanMetrics` (§10.7) is the RED primitive. It compiles to a bucketed aggregate over `spans`:

```sql
SELECT (start_time/step)*step AS bucket, <group cols>,
       count(*) AS calls,
       sum(CASE WHEN status_code='ERROR' …) AS errors,
       approx_percentile_cont(duration_ns, 0.5/0.95/0.99) …
GROUP BY bucket, <group cols>
```

Results materialize into labeled series of `SpanMetricPoint` (calls / errors / error_rate / p50·p95·p99 ns). `SpanMetricsQuery` filters on service / name / kind / status / attr_eq / range / step plus `group_by` attribute keys (e.g. `http.route` for the classic p99-latency-by-route RED dashboard). `approx_percentile_cont` is available in the trimmed DataFusion build (default aggregate functions), so quantiles need no custom UDF — the RED query is pure SQL over the existing spans table, no new storage.

### Trace search by span-name text (`TraceQuery::matches`)

`TraceQuery::matches(text)` (in `imbh/traces.rs`) is a tokenized term search over the span `name` column: a trace is a candidate when any of its spans' names contain all query terms. It adds `matches(name, '…')` to the search `where_sql`, complementing the exact `.name()` filter. The `matches` UDF (registered globally in `session_context`) works row-wise on ANY `Utf8` column via the shared `imbh_core::matches_terms` fallback, so it applies to span `name` even though spans are not registered as a Tantivy text column. Per-segment Tantivy span-name acceleration remains a deferred performance item; results are identical either way because both paths share the tokenizer.

### Trace candidate-selection boundary bug (trace_start over filtered spans)

The one real hazard found in a "per-group aggregate over WHERE-filtered rows" audit: in `TracesApi::search` Phase 1, `min(start_time)` (the `trace_start` HAVING) and `max(start_time)` (recency) were aggregated over the `where_sql`-filtered spans. Combining a span predicate (`name`/`attr`/…) with `trace_start_range` therefore computed the trace start over only the *matching* spans — a trace whose root is in range but whose matching span is later was silently dropped, and traces starting before range with an in-range matching span were spuriously included. This was latent in the public `TraceQuery` API and would have been the exact bug in the planned TraceQL predicate pushdown. (Log `volume` / `span_metrics` / metric `range()` were cleared: those use per-row tumbling buckets, not group-start aggregates.)

Fix: `search()` selects matching traces via a `trace_id IN (SELECT hex(trace_id) FROM spans WHERE <predicate>)` semi-join, and aggregates `min/max(start_time)` over ALL spans of the matched traces (no outer `WHERE`). This keeps trace start = true start, and keeps the predicate in a `WHERE` so the `SqlParams` `$N` bind placeholders still infer/bind. A `SUM(CASE WHEN … = $n …)` form broke DataFusion's parameter type inference; interpolating the value would have been an injection regression, so the parameterized semi-join was chosen. Refactor: `where_sql` -> `span_conditions` (raw conditions); `trace_start_having` stays min-start-only.

Related audit outcome: sliding-window fetch bounds are correct (LogQL `plan_log_fetch` = `[start-offset-window, end-offset]`, PromQL `collect_fetches` = `[start-window, end]`; the window is folded as a backward lookback at the start, nothing in `[end, end+window]` is needed) — pinned by the logql test `windows_are_left_open_right_closed_and_never_reach_past_the_range_end`.

### TraceQL streaming evaluation + sound predicate pushdown

Both changes ride on top of the trace_start fix. (The TraceQL grammar/dialect itself is documented in the imbh-lgtm doc; this covers the query-correctness side.)

**Streaming.** TraceQL is per-trace independent (each trace evaluated in isolation), so `fetch_complete_traces` (which materialized *all* candidate traces into one pack) was replaced by a streaming `TraceSource`: `fetch_candidates` returns candidate ids, then `execute_traceql` pulls each trace via `fetch_trace` (a single-trace `TracePack`), evaluates it, and drops it before the next. Peak memory dropped from the whole candidate set to one trace; output is identical.

**Predicate pushdown.** `candidate_filters(&SpansetExpr) -> Vec<SpanCandidateFilter>` extracts a *necessary* single-span filter and threads it via `TraceFetchRequest.candidate` -> `build_trace_query` -> the fixed semi-join candidate query, so traces that cannot match are skipped in storage. Sound by construction and unit-tested:

- Bare `Select` lifts its pushable conjunction (same span).
- `And` / `Structural` / `count>=1` / `countAtLeast(>=1)` push one necessary side.
- `Or`, `count<=` / `==0` / `countAtLeast(0)`, `Ne` / `Regex` / non-`Span`-scope / numeric / intrinsic leaves push **nothing** (evaluate all).
- Partial pushdown of a conjunction is kept (a subset of a necessary AND is still necessary).

Because it rides the trace_start-correct semi-join, pushing a predicate cannot drop a trace whose root is in range but whose matching span is later, and all user values stay parameterized (`p.str` / `p.i64` / `p.attr_field`). Level 3 (multi-query set algebra) was deliberately not built: extra `search()` scans can cost more than streaming all candidates (per-trace eval is cheap).

Two predicate classes remain deliberately unpushed because they are unsound under the single-span semi-join:

- **Cross-branch `And`/`Structural` unions.** The semi-join requires all conditions to hold on ONE span, so unioning filters from two different spansets (e.g. `{name="a"} && {name="b"}`) would demand one span matching both, dropping real matches. Pushing only one branch is correct.
- **Numeric attribute comparisons** — were blocked by a storage bug (below), now pushable for integer attrs.

### TraceQL numeric-attribute pushdown + a latent facade correctness fix

Root-cause storage bug: OTLP `IntValue`/`DoubleValue` attributes are canonical-JSON-encoded as bare numbers (`{"http.status_code":500}`), but the `json_get_str` UDF returns a value only for `AnyValue::Str` — NULL for a numeric scalar. So `TRY_CAST(json_get_str(...) AS DOUBLE) >= 500` never matched an int-typed attribute. This was not only a pushdown gap: the facade's own `TraceQuery`/`LogQuery` `attr_gt`/`ge`/`lt`/`le` silently failed on integer/double-typed attributes — they only worked on numbers that happened to arrive as JSON strings, and the sole existing numeric test (`logs_numeric_attr_filter`) fed string values, so it never exercised the broken path. The whole chain was confirmed in code: `pb_to_any` (IntValue->AnyValue::Int) -> `encode_value` (writes `500`, not `"500"`) -> `json_get_str_impl` (NULL).

Full fix (root cause, not just the optimization):

1. **imbh-query** — new `json_get_num(json,key)->Float64` UDF: returns the number for an integer, double, or numeric-parseable-string scalar; NULL otherwise. Registered next to `json_get_str`.
2. **imbh (facade)** — `SqlParams::attr_num_field` routes the `attr_num` matchers through `json_get_num` for JSON-blob attributes (promoted columns keep the exact `TRY_CAST(CAST(col AS VARCHAR) AS DOUBLE)` path, so string-encoded numbers do not regress). Fixes logs and traces.
3. **imbh-lgtm** — `SpanCandidateFilter::AttrNum{Gt,Ge,Lt,Le}(String,i64)`; `push_numeric_attr` lifts `.key <op> int` (Eq -> closed `>=v AND <=v`; a `±2^53` guard keeps the `i64->f64` widen lossless so the pushed bound is exact — no rounding could turn a necessary condition into a stricter one; float/duration literals stay with the evaluator); `build_trace_query` translates them to `attr_gt` etc.

Soundness: `json_get_num` returns a value whenever the evaluator would match (int/double), and over-selects at worst for string-encoded numbers — a sound superset, re-checked by the evaluator.

### Trace Level-2 materialization measurement (measured, not built)

Measured the ceiling before building the reader. Added `TracesApi::get_batches` (the trace span scan minus `Span`/`Trace` materialization), an `otlp_trace_wide` fixture, and `examples/trace_level2_ceiling.rs`. One 100-span trace, 5 attrs/span: `get_batches` (SQL scan) = **6,567 allocs** vs `get` (+ materialize + assemble) = **9,175** — DTO materialization is **28% of `get`**, which is the *ceiling*. A raw-Arrow `fetch_trace` would still parse the attributes/resource/scope/events/links JSON (attributes are blobs, like metrics), saving only the struct-building slice — realistically ~10-15% per streamed trace.

Decision: not worth building. Unlike LogQL/metric Level-2 (many rows per query -> materialization dominates -> 1.2-1.7x), traces stream one at a time and each is small, while the trace reader would be the largest of the three (13 span fields + events/links JSON + parent assembly + root computation, duplicating `materialize_spans`/`assemble_trace`/`semantic_trace`). Biggest reader, smallest payoff. The trace path is already efficient: streaming bounds peak memory to one trace, and the pushdown narrows the candidate scan. `get_batches` is kept as the ready prerequisite should the ROI change.

### Typed nested Error model (§10.3)

The string-payload `Error` (`Open`/`Ingest`/`Query`/`Storage`/`Config`(`String`) + `Closed`) was replaced with the §10.3 typed nested model, because the old shape carried too little to diagnose a failure: `impl Error {}`'s empty `source()` flattened every backing io/DataFusion/tantivy/arrow/parquet/prost error into a string (the cause was gone), structure (table/column/path/lsn/byte-count/phase) was stringly-typed, and the classifiers substring-sniffed (`is_backpressure`) or were a hardcoded `false` stub (`is_not_found`).

**New shape (all in `crates/imbh-core/src/error.rs`).** Each category wraps a `*Error` struct `{ pub kind: *Kind, source: Option<Box<dyn std::error::Error + Send + Sync>> }` (struct-per-category so `source()` is a 6-arm match and the `kind` enums stay pure matchable data). Typed leaves grounded in the surveyed sites:

- `QueryKind::{UnknownTable, UnknownColumn, ColumnType{column,expected,actual}, Coerce{column}, Plan, Execute, Search{doc_id}, Message}`
- `StorageKind::{Io{path}, Wal{phase}, Parquet{phase}, PayloadTooLarge{len,limit}, InvalidZstdLevel, BuildBatch{table}, MissingColumn, Message}`
- `IngestKind::{Decode{signal}, QueueFull, Message}`
- `OpenKind::{CorruptManifest{line,detail}, UnsupportedWalSignal{lsn,signal}, LockHeld{path}, Message}`
- `ConfigKind::{MissingDatabasePath, Message}`

Each category has a `Message(String)` catch-all so the long tail migrates mechanically. All types are `#[non_exhaustive]`.

**The core win: `source()` now chains** the backing error (fixed centrally in `From<io::Error>` + per-site `*_ctx`/`query_plan`/`storage_io`/… constructors), and `Display` renders `"<cat> error: <kind>: <source>"` — so the reference server (which serializes `Display` into its JSON body and does not walk `source()`) still carries the real cause, and structured logging gets the typed, downcastable chain. **Classifiers are exact** now (typed-leaf `matches!`, not text): `is_backpressure`->`QueueFull`, `is_not_found`->`UnknownTable`/`UnknownColumn`, `is_user_error` = `Ingest|Query|Config`; signatures unchanged, so `imbh-server` was untouched. `imbh-server::error_response` uses the classifiers for its 4xx/5xx split (404 not-found / 400 user-error / 500 otherwise). The 404 path becomes reachable the moment such a leaf is constructed (no site does yet, so runtime behavior is unchanged today).

**Migration.** Big-bang core flip, then ~100 call sites across the 5 downstream crates migrated in dependency order (`core` -> parallel `otlp`/`index` -> parallel `storage`/`query` -> `imbh`), each gated `-p <crate>` against the finalized core. Nothing needed `Error: Clone`/`PartialEq`; the boxed source is `Send + Sync + 'static` (DataFusion's `df_err` bound — locked by an `error_is_send_sync` compile test). Zero new dependencies (no `thiserror`/`anyhow`; ~30 lines of hand-rolled `Display`/`source`), crate count held at 275. `Error::column_type` was added at the ~15 metrics.rs downcast sites so a result column with the wrong Arrow type reports the column name + expected type as data. `imbh --no-default-features` still compiles.

**Reusable playbook / findings from reshaping a foundational core type:**

1. **Big-bang core, then dependency-ordered parallel migration.** Finalize the new type in `imbh-core` in one pass and let the workspace go red; then migrate the downstream crates each gated `-p <crate>` (never `--workspace` mid-flight), in internal dependency order. The order is not obvious from names: `otlp`/`index` are core-only (first, in parallel); `storage` and `query` both depend on `imbh-index` (after it, in parallel with each other); `imbh` depends on storage/index/query (last). A crate can be made green against the finalized core independently of the others still being red, which is what makes the parallelism safe. A "shim" staging keeps every intermediate green but doubles the core work — big-bang was simpler and fine because we do not commit mid-migration.
2. **A schema/type-signature change MUST gate `cargo test --workspace`, not the per-crate gate.** Second time the lesson bit (first: dict-encode -> exporter test helper). Per-crate gates are green while a downstream consumer — often a *test helper* that downcasts a result column or constructs the old variant — is silently broken.
3. **Struct-per-category + boxed source is the right hand-rolled-error idiom** (no `thiserror`, ~30 lines, zero deps). Why it beats a source field on every leaf variant: `source()` is a 6-arm match; `kind` enums stay pure matchable data; the `Message` catch-all gets a source for free. It mirrors `std::io::Error` (opaque kind + boxed inner). The box must be `Send + Sync + 'static` — lock it with a compile-time `fn _assert<T: Send + Sync + 'static>(){}` test.
4. **A `Message(String)` catch-all per category is what makes a typed taxonomy migratable.** High-value sites get typed leaves with structured fields; the long tail gets a message + chained source. Without it, a typed taxonomy forces hand-classifying every one of ~100 sites — the reason this item sat deferred as "cross-cutting, high-conflict."
5. **`Display` must append `source()` when any consumer serializes `Display` without walking the chain.** `imbh-server` renders `e.to_string()` straight into its JSON body and never calls `source()`.
6. **The blast-radius survey is the precondition that makes a payload reshape safe.** Confirm nothing external `match`/`if let`s the variants and nothing needs `Error: Clone`/`PartialEq`. Here the only variant-dispatch was the classifiers + `Display` (in `error.rs`) and the server via classifiers — so payloads could change freely as long as classifier *signatures* held.
7. **Point-free generic constructors.** `.map_err(Error::query_plan)` works when inference resolves the source type from the `Result`; fall back to `|e| Error::query_plan(e)` when it does not. Watch integer widths at typed-leaf boundaries (tantivy `DocId` is `u32`; the `Search { doc_id: u64 }` leaf needed `u64::from`).
8. **`#[non_exhaustive]` legitimizes reserved-but-unproduced leaves.** `is_not_found`/`is_backpressure` have exact plumbing (`UnknownTable`/`UnknownColumn`, `QueueFull`) but no *producer*; `OpenKind::LockHeld` is forward-looking. Adding producers/leaves later is not a breaking change.

**Follow-up:** the manifest field-parse sites use `corrupt_manifest`, which has no `source` slot, so the caught `ParseIntError` is dropped (line + detail are preserved). Add a source slot to `OpenKind::CorruptManifest` if that cause is ever wanted. Wire the `unknown_table`/`queue_full` producers at the real name-resolution / buffer-cap sites when those land.

## Files

- `crates/imbh-core/src/error.rs` — the typed nested `Error` model, classifiers, `Display`/`source`.
- `crates/imbh-core` — `TraceId::from_hex` / `SpanId::from_hex`; the shared `matches_terms` fallback.
- `imbh` facade `traces.rs` — `TracesApi::get`/`search`/`span_metrics`/`get_batches`; `TraceQuery` (+ `matches`); `SqlParams` (`attr_num_field`, semi-join `span_conditions`/`trace_start_having`); `Span`/`Trace`/`TraceSummary`/`SpanMetricPoint` DTOs.
- `imbh-query` — `hex(binary)->Utf8` UDF (`ScalarUDFImpl`); `matches` UDF; `json_get_str`; `json_get_num(json,key)->Float64` UDF.
- `imbh-lgtm` — TraceQL streaming (`TraceSource`, `fetch_candidates`, `fetch_trace`, `execute_traceql`); `candidate_filters(&SpansetExpr) -> Vec<SpanCandidateFilter>`; `SpanCandidateFilter::AttrNum{Gt,Ge,Lt,Le}(String,i64)`; `push_numeric_attr`; `build_trace_query`; `TraceFetchRequest.candidate`.
- `imbh-server` — `error_response` (4xx/5xx split via classifiers).
- `imbh-test-support::otlp` — `otlp_trace_int_attr` builder.

## Test Coverage

- `traces_search_by_name_matches` — a trace with spans "GET /checkout" + "db query": `.matches("checkout")` finds it, `.matches("nonexistent")` returns nothing.
- `tests/trace_search_boundary.rs` — root at 1000, matching child at 1100, `trace_start_range` [900,1050] -> trace found (pins the trace_start boundary fix).
- `windows_are_left_open_right_closed_and_never_reach_past_the_range_end` (logql) — a sample at `end+1` is never counted; the open-left edge is excluded.
- TraceQL pushdown unit tests over `candidate_filters` (Select / And / Structural / count / Or / Ne / Regex / numeric / intrinsic cases).
- `json_get_num_reads_numbers_regardless_of_json_encoding` (query).
- `numeric_matchers_match_typed_numeric_attributes` (facade regression — ingests genuine IntValue/DoubleValue attrs; fails pre-fix).
- `integer_span_attribute_comparisons_lift_to_numeric_filters`, `unpushable_numeric_span_attributes_stay_with_the_evaluator` (lgtm).
- `traceql_numeric_attr_pushdown_matches_typed_attribute` (end-to-end).
- `bad_sql_error_is_diagnosable` — a query on a missing table is a 400 user error whose `Display` surfaces the DataFusion detail and whose `source()` is the downcastable cause.
- `error_is_send_sync` — compile-time assertion that the boxed source is `Send + Sync + 'static`.
- Level-2 ceiling: `examples/trace_level2_ceiling.rs` + `otlp_trace_wide` fixture (measurement, not a pass/fail test).
- Test totals at various points: workspace 49/49 (trace fixes); per-crate at the Error migration — imbh 52, storage 23, query 9, core 24, index 5, otlp 7, exporter 8.

## Pitfalls

- **trace_start must be computed over ALL spans of a matched trace**, never over the predicate-filtered spans. Filtering first drops traces whose root is in range but whose matching span is later, and spuriously includes traces starting before range. Use the parameterized semi-join `hex(trace_id) IN (SELECT hex(trace_id) FROM spans WHERE <predicate>)` and aggregate `min/max(start_time)` outside the `WHERE`.
- **`SUM(CASE WHEN … = $n …)` breaks DataFusion parameter type inference.** Keep parameterized predicates in a `WHERE` clause so `$N` binds infer; do not interpolate user values (injection regression).
- **Necessity is not sufficiency for pushdown.** A candidate filter must be a predicate the *storage matcher* returns a superset for — not merely a logically necessary condition. The `json_get_str` encoding mismatch made a plausible pushdown silently under-select, and the same mismatch was already a correctness bug in the public typed API. Verify the encode->store->match chain end to end.
- **`json_get_str` returns NULL for numeric JSON scalars.** OTLP int/double attributes are stored as bare JSON numbers, so string-only extraction silently fails numeric `attr_gt`/`ge`/`lt`/`le`. Use `json_get_num` for JSON-blob numeric matchers; tests must feed genuine `IntValue`/`DoubleValue` attributes, not string-encoded numbers.
- **Cross-branch `And`/`Structural` numeric/attr filters must not be unioned into one single-span semi-join** — they would demand one span match both branches and drop real matches. Push only one necessary branch.
- **DF54 `ScalarUDFImpl` has no `as_any` and requires `Eq + Hash`** — derive `PartialEq, Eq, Hash` on the UDF struct.
- **Comparing `FixedSizeBinary` to a `X'...'` binary literal is coercion-fragile** — render ids to hex (`hex(trace_id) = '<hex>'`) and compare strings.
- **A shared-type shape change ends with a full `cargo test --workspace` run**, not just the per-crate gate — downstream test helpers that downcast or construct the old variant are silently broken by per-crate-green migrations.
- **When a consumer serializes `Display` without walking `source()`** (as `imbh-server` does into its JSON body), `Display` must append the source or the error body loses the cause.
- **Verify stale TODO/pushdown premises before acting** — several backlog items were filed with premises that no longer held (e.g. string-attr pushdown already landed). Grep the actual code/binary first.
