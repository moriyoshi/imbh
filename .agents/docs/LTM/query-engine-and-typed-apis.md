# Query Engine and Typed Query APIs

## Summary

`imbh-query` is the only crate that knows DataFusion. It exposes a `SegmentTableProvider` that
bridges the Tantivy full-text index into DataFusion as an `Inexact` row-pruning accelerator, with
Parquet as the correctness ground truth. On top of the same providers, the `imbh` facade builds a
family of thin typed query builders (`LogQuery`, `TraceQuery`, `MetricQuery`, `HistogramQuery`,
`ExpHistogramQuery`, `SpanMetricsQuery`) that each compile to SQL with DataFusion bind parameters, a
uniform attribute-matcher vocabulary, discovery/paging/count/series helpers, configurable attribute
promotion to typed columns, and optional `serde` / `proto` binding surfaces.

## Key Facts

- One query path, two front doors: the typed API is a thin SQL builder over the same
  `SegmentTableProvider` the raw `Db::sql` uses (ARCHITECTURE.md §9.4).
- Full-text matching is done by the `matches` UDF **always**; the Tantivy index is a pure row-pruning
  accelerator. Index build, `search_body`, and the UDF share one `imbh_core::tokenize`, so the index
  is actually exact for body terms — the `Inexact` claim is belt-and-suspenders.
- Typed builders use **DataFusion bind parameters** (`$N` placeholders), not string interpolation.
  The `esc()` escaper was deleted; escaping is now impossible to forget.
- The unified attribute-matcher (`MatchOp`) vocabulary is complete and symmetric across logs and
  traces: `attr_eq` / `attr_exists` / `attr_matches` / `attr_in` / `attr_not_in` /
  `attr_gt`·`ge`·`lt`·`le` / `attr_regex`.
- PromQL label selectors (`=` / `!=` / `=~` / `!~`) are uniform across `MetricQuery`,
  `HistogramQuery`, and `ExpHistogramQuery`.
- Read-side scan stats (`QueryStats`: `segments_scanned` / `segments_pruned` / `rows_scanned` /
  `bytes_scanned` / `used_index`) flow from a shared `Arc<ScanAccum>` to the typed API; `used_index`
  means a segment `.tidx` was actually searched.
- The scan is lazy per-batch: `SegmentTableProvider::scan` returns a `StreamingTableExec` over a
  `PartitionStream`, reading one Parquet batch per `poll_next`.
- Attribute promotion (`Db::builder(...).promote(...)`) materializes chosen OTel attribute keys as
  `Dictionary(Int32,Utf8)` columns; the typed builders auto-dispatch a promoted key to its column via
  `SqlParams::attr_field`.
- Off-by-default `serde` and `proto` features add **zero new third-party runtime crates**.
- Footprint held at ~275 crates / imbhd 31.2 MiB across the whole query-vocabulary build-out ("no new
  dependency" discipline); default runtime graph later measured at 281 crates (`cargo tree -e normal`).

## Details

### Engine internals: provider, RowSelection bridge, UDFs

**`SegmentTableProvider` (`crates/imbh-query/src/provider.rs`).** Formerly `LogsProvider`; generalized
to a per-table provider carrying `text_column: Option<String>` (`Some("body")` for logs,
`Some("name")` for spans, `None` for tables with no index pushdown — matches still correct via the
UDF). `run_sql` takes `Vec<TableInput>` and registers a provider per table; the facade registers all
tables for every query. The provider is table-agnostic — logs drive it identically to spans.

**Tantivy → Parquet `RowSelection` bridge.** `provider.scan()` → `row_selection_for` →
`imbh_index::search_body` / `search_attr_eq` per sealed segment. `row_selection_from_sorted` sums to
exactly the segment row count on every off-by-one class (leading skip, inter-run, tail) and emits no
zero-count selectors. Row ordinals are stored as a Parquet-write-order fast field (not doc order), so
multiple Tantivy / Parquet-row-group boundaries stay aligned. The buffer ∪ segment union can't
double-count or drop rows because `matches` is `Inexact` — a `FilterExec` re-checks every row, and
index hits share the exact tokenizer with the row-wise fallback.

**`matches` `Inexact`/superset contract.** Safe because the index returns an *equal* set, not merely a
superset: pushdown terms and the UDF re-check both call the same `imbh_core::tokenize` /
`matches_terms`, both AND multi-term, and empty-tokenizing queries fall back to full scan + match-all
on both sides. Pushdown only claims `Inexact` for the exact `matches(text_col, <literal>)` shape;
DataFusion always re-applies `Inexact` predicates as a `FilterExec` above the scan.

**UDFs.** `json_get_str(json, key)` parses the canonical-JSON column and returns the string at `key`;
`matches`, `hex`, `histogram_quantile`, and `regexp_like` (DataFusion 54, RE2 / linear-time via the
Rust `regex` crate — no ReDoS; the workspace enables `regex_expressions`). None have a panic path on
malformed/edge input. `matches_impl` (row-wise) is the correctness authority; `search_body` only picks
which Parquet rows are read.

**`coerce` (`imbh-query/src/lib.rs`).** Matches source batches to the table schema **by column name**
(`src.index_of(field.name())`), order-insensitive, returning `Error::Query` on a missing/uncastable
column rather than panicking. This replaced an earlier positional (`batch.column(i)`) mapping that
panicked when a source batch had fewer columns than the schema — the normal path for every sealed
segment because the fast path compared full `Schema` equality *including metadata* and parquet-read
batches carry metadata. The strict-equality fast path still passes the metadata-free buffer snapshot
straight through. Later, `coerce` was extended to **null-fill a missing *nullable* column**
(`coerce_missing` retained for a missing *non-nullable* column) so adding a promoted column never
breaks a query over a pre-promotion segment.

**Trust-boundary guards (defenses against a broken upstream invariant, not bugs).** (L1,
`provider.rs`) if a Tantivy hit ordinal is `>= seg.rows`, `row_selection_for` falls back to a full
scan (+ `debug_assert`) instead of building an out-of-range `RowSelection`. (L2, `imbh-index`)
`search_body` returns an explicit error if a hit's `row` fast field is missing, rather than silently
dropping the hit (an unsound subset the UDF re-filter can't recover).

**SQL portability facts.** `CAST("time" AS BIGINT)` is the portable way to compare the
`Timestamp(ns)` column against an epoch-nanos literal without a timestamp-construction function.
`"time"` must be quoted (it collides with the `TIME` type keyword); the column is lowercase so quoting
preserves the match. Integer time bucketing `(CAST("time" AS BIGINT)/step)*step` is robust and avoids
DataFusion interval-parsing uncertainty around nanosecond precision.

### Engine internals: SQL bind parameters

Typed-query builders use DataFusion bind parameters, not string interpolation. Every user value
(service/metric names, attribute keys+values, `matches`/regex text, time bounds) is a `$N`
placeholder produced by a `SqlParams` collector (`crates/imbh/src/sql.rs`), bound through
`pub(crate) Db::sql_with_params(sql, Vec<ScalarValue>)` → `Query.params` → `run_sql` binds via
`DataFrame::with_param_values`. `run_sql` gained a `params` arg (empty for raw `Db::sql`, so the
public SQL API is unchanged). `esc()` was deleted. ~50 sites across logs/metrics/traces/attrs.

Left interpolated (not injection surfaces): `trace_id`/`span_id` `X'<hex>'` binary literals (machine
hex; the bloom-pruning path extracts them), `LIMIT`/`OFFSET`, the `step` bucket arithmetic, and
identifiers (table/column/alias/aggregate names — the fixed vocabulary).

Durable facts:
- **DataFusion bind parameters survive TableProvider filter pushdown.** `with_param_values`
  substitutes `$N` into the logical plan BEFORE physical planning, so
  `supports_filters_pushdown`/`scan` see literals — a parameterized `matches(body, $1)` still drives
  the index.
- **Placeholder type inference works in the hard positions** — `$N` resolves as a UDF argument
  (`matches`/`json_get_str`/`regexp_like`), in dict-column equality, and as an integer time bound,
  with no CAST hints (the `parameterized_query_spike`). This is what made bind parameters feasible
  rather than a LogicalPlan rebuild.

### Engine internals: read-side scan stats (QueryStats)

A shared `Arc<ScanAccum>` (atomics: `segments_scanned`, `segments_pruned`, `rows_scanned`,
`bytes_scanned`, `index_searched`) is created in `imbh_query::run_sql`, cloned into every table's
`SegmentTableProvider`, written during the scan, and snapshotted after `collect()`. `run_sql` returns
`(Vec<RecordBatch>, ScanStats)`. In the facade, public `Query::collect` is unchanged (discards
stats); `pub(crate) Query::collect_with_stats` carries the snapshot; `logs().query` populates
`QueryStats` from it. `rows_scanned` is counted on the coerced batches actually read — i.e. *after*
the Tantivy `RowSelection` pruned each segment (exactly "rows the `matches`/`json_get_str` UDF had to
evaluate").

- **`used_index` semantics**: changed from "the query had a `matches` predicate" (a misleading proxy)
  to "a segment's `.tidx` was actually searched" — false for buffer-only / in-memory / index-less /
  above-cost-gate queries. One flag set in `scan()`.
- **`bytes_scanned` is materialized-in-memory bytes** (`RecordBatch::get_array_memory_size`), not
  Parquet-on-disk compressed bytes — the only honest number at the provider layer (the segment is
  already decoded to Arrow). Documented on the field so a consumer doesn't mistake it for I/O volume.
- **Multi-table caveat**: the accumulator is shared across *every* registered table's provider, so a
  cross-signal SQL query sums stats across all scanned tables. A single typed query (logs/traces/
  metrics) scans only its own table, so the numbers are per-table as intended.
- **Proving an accelerator is *consulted***: when accelerator and source-of-truth agree by design, the
  answer is identical whether or not the accelerator ran — so make them disagree (a stale `.tidx`) and
  assert the difference. A cross-crate `#[cfg(test)]` counter is invisible to a dependent crate's
  tests and races with parallel tests (an earlier `SEARCHED` counter was reverted; the two
  counter-asserting bloom tests are serialized via a `prune_counters::SERIAL` mutex).

### Engine internals: lazy per-batch scan (prescription I-4a / I-5)

The streaming API was streaming in name only: `SegmentTableProvider::scan` eagerly read every
buffer/segment batch into one `Vec<RecordBatch>`, built a `MemTable`, and returned its plan — so the
whole result was materialized in RAM inside `execute_stream().await` before the first `poll_next`.

**Fix**: use `datafusion::physical_plan::streaming::{PartitionStream, StreamingTableExec}` (DataFusion
54), the repo's first custom plan node. The provider only supplies a `PartitionStream` yielding
**full-schema** batches; `StreamingTableExec::try_new` requires each partition's `schema()` to equal
the unprojected table schema, then applies projection (`b.project(..)`), limit (`LimitStream`, which
stops polling early so a `LIMIT` never reads past segments), and cooperative yielding
(`make_cooperative`) itself in `execute()` — no hand-rolled `ExecutionPlan`/`DisplayAs`/
`PlanProperties`. Pass an empty ordering as `None::<LexOrdering>`.

**Lazy state machine**: `SegmentBatchIter` is a synchronous `Iterator<Item = DFResult<RecordBatch>>`:
emit the buffer batch, then walk segments, opening each `ParquetRecordBatchReader` (via `open_segment`,
which returns `None` when a bloom filter prunes the segment) only when reached and pulling **one**
batch per `next()`. `RecordBatchStreamAdapter::new(schema, futures::stream::iter(sync_iter))` bridges
Iterator → `SendableRecordBatchStream` (no `Unpin` bound needed; it is `pin_project!`-ed).
`ParquetRecordBatchReader` is `Send` (`parquet`'s `trait ArrayReader: Send`), so it lives inside the
stream across polls. `futures` was added as a direct dep of `imbh-query` but is footprint-neutral
(datafusion already pulls it).

**Stats-on-stream (I-5)**: `ScanAccum` counters now accrue during `poll_next` (complete only after the
stream is fully drained). The collect path drains fully before snapshotting, so `ScanStats` and the
prune-counter tests are unaffected. `run_sql_stream` returns `(SendableRecordBatchStream,
StreamStatsHandle)`; the handle holds the same `Arc<ScanAccum>` and `handle.get() -> ScanStats` is
complete after drain. Facade `Query::stream_with_stats` exposes it (`Query::stream` unchanged, drops
the handle). This supersedes the earlier finding "getting read-side stats relies on the provider
reading eagerly" — the capture moved onto the lazy poll path, as that finding anticipated.

Caveat: a facade-level "one poll reads little" test is defeated by `CoalesceBatchesExec`, which pulls
source batches until it has ~`batch_size` (4096) rows before emitting one; with small test data the
first `poll_next` drains everything. Test per-batch laziness by executing the source plan node in
isolation (`provider.scan(...)` → `plan.execute(0, ...)`), not through the coalesced pipeline.

### Typed API surface: Logs

`db.logs().query(LogQuery) -> LogPage` with materialized `LogEntry` DTOs (attributes/resource/scope
parsed via the JSON parser). `LogQuery` is a thin SQL builder: `matches` → the Tantivy-accelerated
UDF; `attr_eq` → `json_get_str(...) = 'v'`; `service` → the promoted column; time range →
`CAST("time" AS BIGINT)` bounds; `direction` → `ORDER BY "time" DESC|ASC`.

- **`logs().volume(filter, step)`** — record counts per `step`-nanos time bucket
  (`floor(time/step)*step` via integer arithmetic on `CAST("time" AS BIGINT)`), over the same filter
  vocabulary as `LogQuery` via a shared `where_sql`, so every matcher works in volume for free.
- **`logs().volume_by(filter, step, &[keys])`** (`imbh/logs.rs`) — record counts per
  `(step-bucket, label set)`, the Loki `/index/volume`-with-labels shape. Each key adds a
  `json_get_str(attributes, 'k') AS gN` group column (keys `esc()`-escaped, so no injection);
  `VolumeBucket` carries a `labels: Vec<(String, String)>` field, and `materialize_volume` reads the
  label columns and the count at index `1 + group_by.len()`. Plain `volume()` delegates to
  `volume_by(.., &[])` (empty labels), so the two share one code path and stay behaviorally identical
  for existing callers. Regression: `logs_volume_by_group` (three logs in one bucket, route=/a ×2,
  route=/b ×1 → two labeled buckets with counts 2 and 1).
- **`logs().count(filter) -> u64`** — `count(*)` over `where_sql`, ignoring `limit`/`direction`/`after`
  (a count has no page). Extracts the Int64 `count(*)` defensively (empty → 0, negative → clamped).
- **Cursor paging (`LogQuery::after`)** — OFFSET-based. `PageCursor` carries the consumed-row count;
  `after(cursor)` resumes; `to_sql` appends `OFFSET {n}`; `query()` returns
  `LogPage.next = Some(PageCursor(offset + len))` when a full page came back (`len == limit`) and
  `None` on a short page. OFFSET was chosen over keyset because log rows carry no stable unique
  identifier (a `time`-keyset cursor would drop/duplicate rows sharing a boundary nanosecond); re-scan
  cost is O(offset), fine for a log viewer. `PageCursor` is documented opaque so a future keyset
  encoding is a non-breaking swap.

### Typed API surface: Metrics

`db.metrics().catalog/range/instant/series` over gauge/sum (and histogram families for series/catalog).

- **`range(MetricQuery) -> Matrix`** — compiles to `SELECT (time/step)*step AS bucket,
  json_get_str(attributes,'k') AS g0…, <agg>(value) FROM <gauge|sum> WHERE metric=… [filters] GROUP BY
  bucket, g0… ORDER BY bucket`, materialized into a `Matrix` of `MetricSeries` (grouped by the
  `group_by` label set, samples ordered by bucket).
- **`instant(...) -> Vector`** — the last sample per series.
- **`catalog() -> Vec<MetricMeta>`** — distinct (metric, unit, temporality) tagged by kind, one query
  per scalar table.
- **`series(metric) -> Vec<Attributes>`** — distinct data-point attribute label sets. `SELECT DISTINCT
  attributes` UNIONed across the metric tables (UNION dedupes), each row parsed via
  `Attributes::from_canonical_json`. Resource-level dimensions (`service`) are separate axes, not
  folded in. This unions **all five** metric tables (a fix from an earlier gauge/sum/histogram-only
  version that silently omitted `metrics_exp_histogram` and `metrics_summary`).
- **`MetricQuery` builder**: `gauge`/`sum` constructors, `aggregation` (avg/sum/min/max/count),
  `group_by`/`filter`, `step`/`range`/`since`. DTOs: `Matrix`/`MetricSeries`/`Sample`/`Vector`/
  `InstantSample`/`MetricMeta`, `Aggregation`. No PromQL query language — builder + SQL is the surface.

### Typed API surface: attribute discovery (Loki-style, cross-signal)

`imbh_core::Table::ALL` is a `[Table; 7]` const (logs, spans, the five metric families) in a stable
order. `attrs.rs` sweeps `Table::ALL` via `across_all_tables(fragment)`, building a 7-way `UNION` from
a per-table `SELECT DISTINCT …` fragment (UNION dedupes across tables).

- **`attrs().names()`** — distinct attribute keys (from the `attributes` blobs, parsed with the shared
  JSON parser) ∪ a `service.name` presence probe, across all tables. Sorted.
- **`attrs().values(key)`** — `service.name` → the promoted `service` column; any other key →
  `json_get_str(attributes, key)` distinct non-null values, across all tables.

Safe because the facade always registers all 7 tables in every query even when empty, so `SELECT …
FROM spans` on an empty spans table yields 0 rows. Discovery is scan-based (`SELECT DISTINCT`), not the
plan's near-free term-dictionary read (ARCHITECTURE.md §10) — correct and simple; `DISTINCT
attributes` keeps JSON-parse cost proportional to distinct attribute blobs, not rows. Per-signal
scoping is one `Db::sql("… FROM logs")` away for Loki-compatible logs-only hosts. Results sorted via
`BTreeSet`.

### Matcher vocabulary (MatchOp): logs + traces, symmetric

The unified attribute-matcher vocabulary is complete and symmetric on both `LogQuery` and
`TraceQuery`, all repeatable and AND-combined via the shared `where_sql`, all keys/values bound as
parameters (formerly `esc()`-escaped), all additive:

| Matcher | Compiles to |
| --- | --- |
| `attr_eq(key, v)` | `<attr_field> = 'v'` |
| `attr_exists(key)` | `<attr_field> IS NOT NULL` |
| `attr_matches(key, text)` | `matches(<attr_field>, 'text')` (tokenized term-search, reuses the `matches` UDF) |
| `attr_in(key, &[v…])` | `<attr_field> IN ('v1','v2',…)`; empty set → `1 = 0` (matches nothing) |
| `attr_not_in(key, &[v…])` | `(<attr_field> IS NULL OR <attr_field> NOT IN (…))` (NULL-aware — a row lacking the key is kept); empty set → excludes nothing |
| `attr_gt`/`attr_ge`/`attr_lt`/`attr_le(key, n)` | `TRY_CAST(<attr_field> AS DOUBLE) <op> <n>` (TRY_CAST yields NULL → row excluded for non-numeric/missing) |
| `attr_regex(key, pattern)` | `regexp_like(<attr_field>, '<pattern>')` (RE2, no ReDoS); NULL input never matches |

`<attr_field>` is `json_get_str(attributes, $key)` for an unpromoted key or `CAST("key" AS VARCHAR)`
for a promoted key (see promotion below). The numeric operators are stored as `pub(crate) enum NumOp {
Gt, Ge, Lt, Le }` with `as_sql() -> &'static str` (an owned enum was required because `&'static str`
cannot `Deserialize`). `attr_matches` on `/checkout` matches because the tokenizer splits `/checkout`
→ `checkout`.

### Matcher vocabulary: PromQL label selectors on metric queries

`MetricQuery`, `HistogramQuery`, and `ExpHistogramQuery` share the full PromQL label-selector set via a
`label_cond(key, op, value)` helper + `LabelOp` enum:

- `filter` (`k="v"`), `filter_ne` (`k!="v"`), `filter_regex` (`k=~"pat"`), `filter_not_regex`
  (`k!~"pat"`).
- Positive ops (`=`/`=~`) exclude a series missing the label; negation ops (`!=`/`!~`) are NULL-aware
  and KEEP a series missing the label (matching PromQL's absent-label = "" semantics). Regex via
  `regexp_like` (RE2).
- The internal `filters` field is `Vec<(String, LabelOp, String)>` on all three structs; the filter
  SQL loop is the byte-identical `for (k, op, v) in &self.filters { conds.push(label_cond(k, *op, v));
  }`. `filter()`'s public signature is unchanged.

### Attribute promotion to typed columns (ARCHITECTURE.md §6.1)

`Db::builder(...).promote(Promote::new(["http.route", ...]))` lifts chosen OTel attribute keys to real
`Dictionary(Int32,Utf8)` columns at rest. Motivation: labels-as-JSON-blobs was the reason "eval in
engine" bought no zero-copy (`json_get_str` mints fresh strings); promotion makes labels real Arrow
dictionary buffers, the precondition for SQL pushdown.

On-disk-permanent design decisions:
- **Keep-in-JSON, not move.** The promoted key stays in the canonical-JSON `attributes` blob; the
  column is a materialized projection, not a relocation — so `json_get_str` and every existing query
  stay correct. Stripping later is always possible; un-stripping old segments is not.
- **Derive from JSON at buffer-encode.** The 6 `*_rows_to_batch` builders project each promoted column
  from the row's already-present JSON via `imbh_core::json_get`. This avoided touching `imbh-core` row
  structs and the `imbh-otlp` normalizers.
- **record `attributes` scope only** (`lookup_promoted` reads just `attributes`, not a merged
  record→resource→scope projection) — this makes the column byte-identical to `json_get_str(attributes,
  key)`, so dispatch is provably equivalent. Resource promotion isn't lost: `service` is the promoted
  resource attribute with its own column, and `resource` is dict-encoded/cheap to scan.
- **Uniform, append-only, dict-typed.** Columns are appended after each signal's fixed columns (so
  facade positional `batch.column(N)` readers don't shift); the same set is added to all 6 schemas.
  Collisions with built-in names are dropped (`RESERVED_COLUMNS` + `promoted_columns()` in schema.rs,
  shared by schema construction and the builders so they can't disagree).

**Pushdown dispatch (Stage 3).** `SqlParams` carries the effective promoted columns
(`SqlParams::with_promote(promote)`); `SqlParams::attr_field(key)` returns `CAST("key" AS VARCHAR)`
for a promoted key (exactly how `service` is read), else `json_get_str(attributes, $key)`. Each `*Api`
entry point swaps `SqlParams::new()` → `with_promote(self.db.storage.promote().keys())`, and every
attribute site in `logs.rs`, `metrics.rs`, `traces.rs`, `attrs.rs` calls `p.attr_field(k)` — nearly
signature-free.

**Threading.** `Storage` gained a `promote: Promote` field + `with_promote()` consuming builder (set
by the facade right after construction, before replay); every `*_schema()` / `*_rows_to_batch` /
`push_*_batch` takes `&[String]`; `DbBuilder::promote()` plumbs it to all three `Storage` constructors
and to the read-only scratch buffers.

**Stage 4 — LGTM label-read zero-copy: analyzed, not pursued (bounded).** Real zero-copy on the label
*read* side does not pay off: (1) `metric_labels` maps every string attribute to a label, so PromQL
needs the whole `attributes` map — JSON is parsed regardless of what is promoted; (2) the facade
materializes the parsed map into the public `MetricPoint.attributes`/`LogEntry.attributes` DTO fields
unconditionally; (3) the reference evaluators build an owned `LabelSet(Vec<(String,String)>)`. The
promotion payoff is on the **filter/pushdown** side (Stages 1-3), not the label-read side. Revisit only
behind a concrete driver and a human decision on the `LabelSet` API.

### JSON parser (imbh-core)

A small dependency-free recursive-descent JSON parser (`json.rs`), the exact inverse of the canonical
encoder: objects/arrays/strings (`\uXXXX`)/numbers/bool/null and the `{"$f":…}` non-finite sentinel →
`Double`. Round-trip tested against `canonical_json_value`. Plus `Attributes` (owned map with typed
accessors, `Attributes::from_canonical_json`), `json_get`, `SeverityNumber`,
`TimeRange`/`Direction`/`DurationNs`, `Timestamp::now`.

- **Depth guard**: a `depth` counter bounded at `MAX_DEPTH = 128` at the single container-recursion
  point in `value()` (increment on `{`/`[`, `None` past the cap, decrement after). Prevents
  stack-overflow on pathologically nested input (unreachable in the pipeline — all parsed JSON is
  IMBH's own depth-limited canonical output).
- **Documented limits**: `\uXXXX` surrogate pairs are not recombined (the canonical encoder never
  emits them); base64 `bytes` come back as `Str` (a JSON string has no type tag) — fine for attribute
  display; columns remain byte-exact on disk. Truncated/invalid slices → `None`; i64→f64 overflow
  falls back.
- `imbh-query`'s `json_get_str(json, key)` UDF parses this canonical-JSON column and returns the
  string at `key`, registered alongside `matches`.

### Binding surface: serde feature (JSON DTOs, ARCHITECTURE.md §10.13 / §11)

Off-by-default `imbh/serde` feature (forwarding to `imbh-core/serde`) derives `Serialize`/`Deserialize`
for the typed query builders and result DTOs. Gated `#[cfg_attr(feature = "serde", derive(...))]`.

- Builders + operator enums: `LogQuery`, `TraceQuery`, `SpanMetricsQuery`, `MetricQuery`,
  `HistogramQuery`, `ExpHistogramQuery`, `Aggregation`, `RateMode`, `LabelOp`.
- Result DTOs: `LogPage`/`PageCursor`/`QueryStats`/`LogEntry`/`VolumeBucket`,
  `Trace`/`TraceSummary`/`Span`/`SpanMetricPoint`/`SpanMetricSeries`/`SpanMetrics`,
  `Exemplar`/`Sample`/`MetricSeries`/`Matrix`/`InstantSample`/`Vector`/`MetricMeta`.
- `imbh-core` embedded types: `Timestamp`, `DurationNs`, `Direction`, `TimeRange`, `Table`,
  `SeverityNumber`, `Attributes`, `AnyValue` (derived); `TraceId`/`SpanId` have **manual** impls →
  lowercase-hex strings via `to_hex`/`from_hex` (OTel wire form; malformed hex is a serde error, not a
  truncated array; Deserialize uses `Cow<'de, str>` to work on borrowing and owning deserializers).
- Out of scope: `IngestReceipt`, `DbStats`/`TableStats`, `MaintenanceReport`, `LogRow`, `Lsn`,
  `Signal`, `MetricKind`.

serde / serde_derive / serde_json are already compiled transitively (tantivy via `sketches-ddsketch`,
datafusion, opentelemetry-proto), so the feature adds **zero new crates**. Off by default preserves the
serde-free default graph and the `AnyValue` "serde-independent by default" stance.

### Binding surface: proto feature (protobuf inputs + Arrow-batch outputs, §10.17)

Phase 0 of an out-of-process language binding (a Go binding is the motivating case). Design split:
**protobuf for the small nested query *inputs*, Arrow for the columnar *results*** (zero-copy preferred
over ABI simplicity).

- **New crate `imbh-proto`** holds protobuf wire types for the six query builders + shared types/enums,
  generated from `crates/imbh-proto/proto/imbh/v1/query.proto` by **protox** (pure-Rust compiler) in a
  `build.rs` — no system `protoc`, hermetic/offline. prost-only runtime dep; protox/prost-build are
  build-time only. protox 0.9 / prost-build 0.14 pair with the workspace's prost 0.14. prost's enum
  codegen strips the prefix and PascalCases (`DIRECTION_BACKWARD` → `Direction::Backward`, `NUM_OP_GE`
  → `NumOp::Ge`), matching the domain enum variants 1:1 (verified against generated `OUT_DIR/imbh.v1.rs`,
  not assumed).
- **`imbh` `proto` feature** (`= [dep:imbh-proto]`): `TryFrom<imbh_proto::X>` for each builder in
  `proto_impl.rs`, going entirely through the builders' **public setters** (+ the `pub(crate)`
  `PageCursor`) so they never touch private field layout. Wire→domain narrowing (enum discriminant,
  severity > 255, negative duration, `usize` overflow) is validated as a user error. Facade `pub mod
  proto` re-exports the wire types (`imbh::proto::LogQuery`) + `encode_query_stats`; a name-collision
  between the glob'd wire types and the domain imports forced a two-module split (facade `proto` globs;
  private `proto_impl` uses `pb::`). `TimeRange`/`Direction` conversions are **free helper fns** (the
  orphan rule: both the trait input and `imbh_core::TimeRange` are foreign to `imbh`).
- **Arrow-`RecordBatch` query entry points** — `LogsApi::query_batches_with_stats`,
  `MetricsApi::range_batches`, `TracesApi::span_metrics_batches`: same SQL as their DTO twins but stop
  at `collect_with_stats`, returning `(Vec<RecordBatch>, QueryStats)` and skipping row-DTO
  materialization — what a binding exports zero-copy via the Arrow C Data Interface. The FFI `cdylib` +
  Go package are a separate project. Histogram-quantile and trace-assembly batch variants deferred
  (UDF/Rust-side reshaping).
- **`query_batches` vs `query_batches_with_stats` (name split).** The logs pair collided: an *ungated*
  `LogsApi::query_batches -> Vec<RecordBatch>` (added for the `imbh-lgtm` Level-2 read path) and a
  `#[cfg(feature = "proto")]` twin returning `(Vec<RecordBatch>, QueryStats)` shared one name, so
  enabling `proto` at all — `cargo build --workspace --all-features` included — failed with **E0592
  (duplicate definitions)**. The proto-gated stats variant was renamed to `query_batches_with_stats`;
  the ungated `query_batches` keeps its name because `imbh-lgtm` (`src/source.rs`) and the
  `logql_level2` example bind to it. No gating change was needed on `QueryStats` itself — it is
  unconditional (`LogPage.stats` uses it). Lesson: a feature-gated method is only distinguishable from
  an ungated one by *name*, never by the gate.

**Footprint**: `--features proto` adds exactly one runtime crate — first-party `imbh-proto` — since
prost is already compiled transitively. Use `cargo tree -e normal` for the footprint gate: `-e no-dev`
misleadingly shows +19 because it counts protox's build-deps.

Design rationale (§10.17): Arrow IPC is the wrong shape for query **inputs** (one small nested
heterogeneous struct → a 1-row batch with nested `List<Struct>`/`Union` columns, all overhead, and not
a serde format) but the right shape for tabular **results**. Hence protobuf-in / Arrow-out. For metric
range the SQL already emits the labels-as-columns shape (`bucket, g0..gN, value`), so no reshaping is
needed.

### Api handles

`LogsApi`/`TracesApi`/`MetricsApi`/`AttrsApi` own an `Arc`-backed `Db` handle: `{ db: Db }` (dropping
the `<'a>` lifetime); accessors do `self.clone()`, a refcount bump — `Db` is a `#[derive(Clone)]`
newtype over `Arc<Inner>`. Returned namespaces are `'static` (storable / movable into a spawned task).
Use owned `Db`, not `Arc<Db>` (which would double-wrap the Arc already inside `Db`).

## Files

- `crates/imbh-query/src/provider.rs` — `SegmentTableProvider`, `row_selection_for`,
  `row_selection_from_sorted`, `SegmentBatchIter`, `open_segment`, `StreamingTableExec`/`PartitionStream`
  scan, `ScanAccum`, trust-boundary guards L1.
- `crates/imbh-query/src/lib.rs` — `run_sql` (`params` arg, `ScanStats` return), `run_sql_stream`
  (`StreamStatsHandle`), `coerce`/`coerce_missing`, UDFs, `plan_query`, `#[cfg(test)] mod plan_shape`.
- `crates/imbh-index` — `search_body`/`search_attr_eq` (trust-boundary guard L2).
- `crates/imbh-core/src/json.rs` — dependency-free JSON parser, depth guard; `Attributes`,
  `Attributes::from_canonical_json`, `json_get`, canonical encoder; `Table::ALL`; `TraceId`/`SpanId`
  serde impls.
- `crates/imbh/src/sql.rs` — `SqlParams`, `SqlParams::with_promote`, `SqlParams::attr_field`, `$N`
  collector.
- `crates/imbh/src/logs.rs` — `LogsApi`, `LogQuery`, `LogEntry`/`LogPage`/`QueryStats`/`PageCursor`,
  `where_sql`, `NumOp`, `count`, `volume`, cursor paging.
- `crates/imbh/src/metrics.rs` — `MetricsApi`, `MetricQuery`/`HistogramQuery`/`ExpHistogramQuery`,
  `label_cond`/`LabelOp`, `range`/`instant`/`catalog`/`series`, DTOs.
- `crates/imbh/src/traces.rs` — `TraceQuery` matcher suite, span-metrics.
- `crates/imbh/src/attrs.rs` — `AttrsApi`, `across_all_tables`, `names`/`values`.
- `crates/imbh/src/proto_impl.rs` + facade `pub mod proto` — proto→builder `TryFrom`, `query_batches`
  entry points, `encode_query_stats`.
- `crates/imbh-proto/proto/imbh/v1/query.proto` + `build.rs` (protox) — wire types.
- `crates/imbh-storage/src/schema.rs` — `RESERVED_COLUMNS`, `promoted_columns()`, `lookup_promoted`,
  `*_rows_to_batch`; `Storage::with_promote`, `Promote`, `DbBuilder::promote`.

## Test Coverage

- `imbh-query`: `parameterized_matches_consults_the_logs_index` (stale-`.tidx` divergence proves the
  index is consulted, not just correct); `parameterized_query_spike` (placeholder type inference);
  `scan_reads_one_segment_per_poll` (executes the provider's own plan directly; 0 segments at build,
  1 of 3 per poll, all 3 at drain — regression guard on the lazy path); `coerce_matches_by_name_and_
  errors_on_missing_column`; `coerce_null_fills_a_missing_nullable_column`; `point_lookup_prunes_
  nonmatching_segments`; `mod plan_shape` (4 tests: scan is `StreamingTableExec` not `MemoryExec`;
  `LIMIT n` → `fetch=n`; `SELECT body` → `projection=[body]`; `matches` leaves a `FilterExec` above
  the scan). `cargo test -p imbh-query` = 16 passed.
- `imbh-core`: `depth_guard_rejects_deep_nesting_without_overflow` (5000-deep `[[…]]` → `None`);
  canonical JSON round-trip; serde `TraceId`/`SpanId` hex + malformed-hex rejection.
- `imbh` facade: `typed_logs_query_api` (buffer rows → `!used_index`); `query_stats_report_index_
  pruning` (sealed segment, selective `matches` → `used_index`, `segments_scanned == 1`, `rows_scanned
  == 1` after 4-row RowSelection prune); `stream_with_stats_reports_scan_counters_after_drain` (0
  before poll, correct after drain); `logs_cursor_paging` (5 rows at limit 2 → 2/2/1, cursor
  terminates); `logs_attr_matchers` (exists/matches/in/count); `logs_numeric_attr_filter` (all four
  operators + range, non-numeric/missing excluded); `attr_regex` cases; `traces_attr_matchers`
  (symmetric matchers); `metrics_promql_label_selectors` (`=`→1/`!=`→2/`=~"^web"`→2/`!~`→1);
  `metrics_series_lists_label_sets`, `metrics_series_spans_all_tables`; `attribute_discovery_is_cross_
  signal`; `promoted_attribute_is_a_typed_column_and_stays_in_json`, `no_promotion_leaves_the_schema_
  unchanged`, `promoted_key_filter_matches_the_json_scan_result`; serde round-trip tests
  (`#[cfg(all(test, feature = "serde"))]`); 4 proto tests (`#[cfg(feature = "proto")]`).
- Test-data builders added along the way: `otlp_gauge_labeled` (data points carry a `{key:val}`
  attribute), `otlp_span_attr` (one attributed span) — the default metric/trace builders emit
  attribute-less points.

## Pitfalls

- **`matches` alone never guarantees the index ran.** Because the predicate is `Inexact`, DataFusion
  keeps the UDF as a `FilterExec` above the scan and re-checks, so a silent full-scan yields identical
  counts. To prove *consultation* you need an observable divergence (a stale `.tidx` that omits a
  matching row the Parquet keeps).
- **`CoalesceBatchesExec` masks per-poll laziness** in facade tests (it drains ~4096 rows before
  emitting). Test laziness against the source plan node in isolation.
- **A `pub fn` exposing a `pub(crate)` type is a `clippy::private_interfaces` error under `-D warnings`
  even though `cargo build`/`cargo test` only *warn*** — the workspace test suite can pass while
  `clippy --workspace` fails. `SegmentTableProvider::new` was made `pub(crate)` (rather than leaking
  `ScanAccum`). The clippy gate catches API-privacy leaks the test gate tolerates.
- **`cargo tree -p imbh -e no-dev` still counts build-dependencies** (protox/prost-build/logos/miette
  via a build.rs) — it showed a false +19 for `imbh-proto`. Use `cargo tree -e normal` (excludes both
  dev and build deps) for "does this add crates".
- **`&'static str` cannot `Deserialize`.** Any serde/proto work on a builder with a borrowed-`'static`
  field (e.g. the old `Vec<(String, &'static str, f64)>` numeric operators) requires an owned enum
  (`NumOp` with `as_sql()`).
- **`pub use imbh_proto::*` collides** with domain-type imports of the same names — forces a two-module
  split whenever a module both glob-re-exports a foreign crate and names local types.
- **A `#[cfg(feature = …)]` method does not "overload" an ungated one of the same name.** The
  `LogsApi::query_batches` / `query_batches_with_stats` split existed because the gated + ungated pair
  compiled together under `--all-features` and hit E0592. `cargo build --workspace` (default features)
  will not catch this class of defect — only a feature-matrix build will, so run
  `cargo build --workspace --all-features` after adding any feature-gated inherent method.
- **`attr_not_in` must be NULL-aware**: a bare `NULL NOT IN (…)` evaluates to NULL and would wrongly
  drop a row that lacks the key; compile to `(<attr_field> IS NULL OR <attr_field> NOT IN (…))`.
- **PromQL negation selectors (`!=`/`!~`) must keep label-absent series** (PromQL absent-label = "");
  positive selectors exclude them.
- **`used_index` as "query had a matches predicate" is a footgun** — it reports "index used" for a
  query that full-scanned the buffer. Tie it to real `.tidx` consultation.
- **Promoted columns are on-disk-permanent and must be byte-identical to `json_get_str(attributes,
  key)`** for dispatch to be equivalent — use record `attributes` scope only, not a merged
  record→resource→scope projection.
- **Promoted columns are appended after fixed columns** so facade positional `batch.column(N)` readers
  don't shift; adding a nullable column also requires `coerce` to null-fill missing nullable columns
  or every query over a pre-promotion segment breaks.
- **Environment**: the full-workspace build was OOM-killed once during heavy parallel compilation
  (datafusion + test binaries). Running tests per-crate with `--jobs 4` avoided a repeat.
- **If the query path is ever made fully lazy in the collect direction too**, stats capture already
  lives on the poll path — but any consumer reading `ScanStats` before the stream is drained gets
  incomplete counters.
