# OTLP Normalization and the Metrics Data Model

## Summary

imbh decodes OTLP export requests into normalized, stateless hand-off row types in `imbh-core`, extracted by target-table-specific functions in `imbh-otlp` that share a common identity/attribute normalization. All five OTLP metric types (gauge, sum, explicit histogram, exponential histogram, summary) plus spans ingest, persist (Parquet segments + WAL replay), and query via SQL and typed builders, giving seven query tables: `logs`, `spans`, `metrics_gauge`, `metrics_sum`, `metrics_histogram`, `metrics_exp_histogram`, `metrics_summary`. Quantile math lives arrow-free in `imbh_core::histogram` and is shared by a DataFusion UDF and safe Rust-side typed-API materialization; rates, cross-series merges, and exemplars round out the query surface.

## Key Facts

- Decode is stateless: `imbh-core` row types are the ingest hand-off; `imbh-otlp` functions turn a decoded request into `Vec<Row>` per target table. Every signal reuses the same resource/scope/attribute helpers (`kvs_to_pairs` / `scope_to_json` / `service_name` / `canonical_json_object` / `temporality_str` / `nonzero`).
- The WAL stores **raw OTLP bytes + a signal tag** and re-derives all row kinds on replay by re-decoding — so adding a new metric table or a new column needs **no WAL frame-format change** and no migration.
- One OTLP metrics request is decoded **once** (`decode_metrics_request`) and all four row kinds (scalar, explicit histogram, exp histogram, summary) are extracted from the shared decoded request; a single ingest carries them under **one WAL frame / LSN**.
- Storage seam: scalar rows share `metric_buffers: BTreeMap<Table, Vec<ScalarMetricRow>>`; each List-column metric type has its **own buffer** but **shares** the schema-agnostic `metric_segments: BTreeMap<Table, Vec<SegmentRef>>` map — so retention, snapshot, manifest persist/load, path enumeration, stats, and compaction cover new tables "for free".
- Quantile math is pure and arrow-free in `imbh_core::histogram` (`histogram_quantile`, `exp_histogram_quantile`), shared by the DataFusion `histogram_quantile` UDF and by the typed API's Rust-side merge — no error-prone `Accumulator` / `ScalarValue::List` aggregate-UDF state protocol.
- Rate is temporality-scoped by explicit method: `.rate()` for delta sums (`sum(value)/step_seconds`), `.rate_counter()` for cumulative monotonic counters (`(max(value)-min(value))/step_seconds`).
- List columns (`List<Float64>`, `List<UInt64>`) and `Int32` offsets round-trip through Parquet and the query `coerce` step (generic `arrow::compute::cast`) with no special handling.
- Adversarial-input hardening (clamp / cap / `saturating_add`) is exact for valid OTLP and only degrades (NaN / collapsed bucket) on malformed input.

## Details

### Spans (traces pillar)

`imbh_core::SpanRow` is the normalized `spans` hand-off (ARCHITECTURE §6.3): trace/span ids, name, `kind` and `status_code` as OTel strings (`SERVER` / `ERROR` / …), `start_time` / `duration_ns`, status message, service, canonical-JSON `attributes` / `resource` / `scope` / `events` / `links`, `trace_state`, flags.

`imbh_otlp::{decode_traces_to_rows, traces_request_to_rows}` turn `ExportTraceServiceRequest` into `Vec<SpanRow>`, reusing the logs helpers. `duration_ns = end − start` (saturating); kind/status are mapped i32 → string; events/links are serialized as canonical-JSON arrays of objects with link ids as lowercase hex.

Decisions: kind/status are stored as **strings** matching the planned `Dict(Utf8)` columns (the typed `SpanKind` / `StatusCode` enums arrive with the `traces()` API); events/links are kept **inline** as canonical JSON rather than as a child table.

Note: opentelemetry-proto 0.32's `Status` has exactly `{message, code}` — a `..Default::default()` on its literal is a clippy `-D warnings` failure (no remaining fields).

### Scalar metrics — gauge and sum

`imbh_core::ScalarMetricRow` is the ingest hand-off for the `metrics_gauge` / `metrics_sum` tables (ARCHITECTURE §6.4): `table` (Gauge|Sum), time / start_time, metric, unit, service, canonical-JSON attributes/resource/scope, flags, `value: f64`, and `temporality` / `is_monotonic` (sum only), plus an `exemplars: String` column (see Exemplars).

`imbh_otlp::{decode_metrics_to_rows, metrics_request_to_rows}` map `ExportMetricsServiceRequest` to gauge + sum data points. `AsInt` / `AsDouble` → `f64`; sum `aggregation_temporality` i32 → `DELTA` / `CUMULATIVE`. Histogram / exp-histogram / summary points are skipped by this extractor (separate extractors own them).

Storage: `Inner` holds `metric_buffers: BTreeMap<Table, Vec<ScalarMetricRow>>` and `metric_segments: BTreeMap<Table, Vec<SegmentRef>>`. `ingest_metrics` routes rows by `row.table`; `seal` iterates the map (one segment per non-empty table); `retain` includes metric segments; the manifest tags each line with its table name (`metrics_gauge` / …), parsed back via `table_from_manifest_name`. `Table` derives `Ord` / `Hash` so it can key the maps. The shared `metric_scalar_schema` is time / start / metric / unit / service / attrs / resource / scope / flags / `value:Float64` / `temporality:Utf8?` / `is_monotonic:Bool?` / `exemplars:Utf8` — gauge rows leave temporality / is_monotonic null; `scalar_metrics_rows_to_batch` builds it.

Facade: `Db::ingest_otlp_metrics` / `try_ingest_otlp_metrics`; open-time replay dispatches `SIGNAL_METRICS`; `Query::collect` registers `metrics_gauge` / `metrics_sum` from the exported `SCALAR_METRIC_TABLES`.

**Delta→cumulative normalization is deferred to ingest** — it is the one stateful piece (a running accumulator keyed by series identity, checkpointed against the WAL watermark, §6.4), so it belongs in `imbh-storage`, not the stateless decode. Storage stores temporality verbatim.

### Explicit-bucket histograms — `metrics_histogram`

**Data model + OTLP extraction.** `imbh_core::HistogramRow` (§6.4) is the explicit-bucket point. Identity fields (`metric` / `service` / `attributes` / `resource` / `scope`) mirror `ScalarMetricRow`; it adds `count`, optional `sum` / `min` / `max`, `explicit_bounds: Vec<f64>` (N ascending upper bounds) and `bucket_counts: Vec<u64>` (N+1 counts, last = `+Inf` overflow), plus `temporality` and `exemplars: String`. `approx_bytes` accounts for the two vecs. `imbh_otlp::metrics_request_to_histogram_rows(req)` + `decode_metrics_to_histogram_rows(bytes)` + the `histogram_row` helper handle `metric::Data::Histogram`. The invariant is `bucket_counts.len() == explicit_bounds.len() + 1`.

**Storage.** `histogram_schema()` defines the `metrics_histogram` Arrow schema; `explicit_bounds` (`List<Float64>`) and `bucket_counts` (`List<UInt64>`) are List columns whose child fields use `Field::new_list_field(dt, true)` so they match **exactly** what `ListBuilder` emits (child named `item`, nullable) — hand-writing `Field::new("item", dt, false)` would mismatch and `RecordBatch::try_new` rejects on the child-field mismatch. Histogram rows live in a separate `Inner::histogram_buffer: Vec<HistogramRow>` (the List-column row type can't share the scalar `metric_buffers`), but segments reuse the shared `metric_segments` map under `Table::MetricsHistogram`. Supporting fns: `replay_histograms`, `schema_histogram`, `buffer_snapshot_histogram`, `segments_histogram` / `segment_paths_histogram`, `write_histogram_segment`, `histogram_rows_to_batch` (ListBuilder). Facade registers `metrics_histogram` as the fifth SQL table and allows `export` of it.

**Quantile UDF.** `imbh-query`'s `histogram_quantile(phi, explicit_bounds, bucket_counts) -> Float64` is a custom `ScalarUDFImpl` (like `HexUdf`; `Signature::any(3)` since the args are List columns), registered in `session_context`. It estimates the phi-quantile of **one** explicit-bucket data point with Prometheus-style linear interpolation inside the matched bucket. The pure fn returns `NaN` for empty, `-inf` / `+inf` for phi outside `[0,1]`, clamps to the largest finite bound in the `+Inf` overflow bucket, and uses first-bucket lower bound = `min(bounds[0], 0)` (non-negative observation convention). It broadcasts a scalar phi via `to_array_of_size` and tolerates NULL rows. OTLP `bucket_counts` are **per-bucket** (not cumulative like Prometheus `le` series); the walk accumulates them into a running cumulative to find the rank's bucket. `ScalarFunctionArgs::number_rows` gives the output length for scalar-broadcast; `columnar_to_array` / `columnar_to_array` centralizes the scalar→array broadcast.

**Typed surface.** `imbh/metrics.rs` `HistogramQuery` builder (`new(metric)`, `.quantile(phi)` default p95, `.group_by(key)`, `.filter(k,v)`, `.range` / `.since`) + `metrics().histogram_quantile(q) -> Matrix`. Per-data-point mode compiles to:

```
SELECT CAST("time" AS BIGINT) AS t, json_get_str(attributes,'k') AS g0…,
       histogram_quantile(<phi>, explicit_bounds, bucket_counts) AS v
FROM metrics_histogram WHERE … ORDER BY t
```

reusing the existing `materialize_matrix` (its bucket/labels/value column shape matches). `{:?}` on the f64 phi renders a guaranteed float literal (`1.0`, `0.95`) so the SQL plan types the arg as Float64 cleanly.

**Cross-series/time merge.** The pure `histogram_quantile(phi, bounds, counts)` math was moved out of `imbh-query` into an arrow-free `imbh_core::histogram` module so the UDF and the typed API share one implementation. `HistogramQuery::step(Duration)` switches to `merged_sql`, which fetches the raw `bucket` / labels / `explicit_bounds` / `bucket_counts` rows; `materialize_merged_quantile` groups by (time-bucket, label set), **sums the count vectors element-wise** (growing to the longer vector, bounds from the first row), then applies `imbh_core::histogram_quantile` per group — the sound PromQL `sum by (le)` step (merge distributions, then take the quantile). The Rust-side merge sidesteps the aggregate-UDF risk; all matching rows flow to the client before merging, which is fine for bounded dashboard queries.

### Exponential histograms — `metrics_exp_histogram`

**Data model + OTLP extraction.** `imbh_core::ExpHistogramRow` (§6.4) is the base-2 histogram point. Beyond the shared identity + count/sum/min/max/temporality fields it carries `scale` (boundaries at `base = 2^(2^-scale)`), `zero_count` + `zero_threshold`, the positive/negative bucket ranges each as `offset: i32` + `counts: Vec<u64>`, and `exemplars: String`. `approx_bytes` accounts for both count vecs. `imbh_otlp::metrics_request_to_exp_histogram_rows` + `decode_metrics_to_exp_histogram_rows` + the `exp_histogram_row` helper handle `metric::Data::ExponentialHistogram`, flattening the optional `positive` / `negative` `Buckets` (offset + counts) with `(0, vec![])` defaults when absent.

**Storage.** `exp_histogram_schema()` adds `scale` / `positive_offset` / `negative_offset` as `Int32`, `zero_count` (`UInt64`), `zero_threshold` (`Float64`), and `positive_counts` / `negative_counts` as `List<UInt64>` (via `Field::new_list_field`). A separate `exp_histogram_buffer: Vec<ExpHistogramRow>`; segments reuse the shared `metric_segments` map under `Table::MetricsExpHistogram`. Supporting fns mirror the explicit-histogram set: `replay_exp_histograms`, `schema_exp_histogram`, `buffer_snapshot_exp_histogram`, `segments` / `segment_paths_exp_histogram`, `write_exp_histogram_segment`, `exp_histogram_rows_to_batch` (Int32 + List builders). Facade registers `metrics_exp_histogram` as the sixth SQL table; `catalog()` reports kind `exponential_histogram`. Parquet round-trips the Int32 offsets and `List<UInt64>` bucket vectors with no special handling.

**Quantile.** `imbh_core::histogram::exp_histogram_quantile(phi, scale, zero_count, positive_offset, positive_counts, negative_offset, negative_counts)` reconstructs boundaries from `scale` (`base = 2^(2^-scale)`, `bound(i) = 2^(i·2^-scale)`) and walks the rank in ascending-value order: negative buckets (most-negative first = highest index down), the zero bucket, then positive buckets (lowest index up), interpolating linearly within the matched bucket. Same NaN / ±inf edge-case contract as `histogram_quantile`. `imbh/metrics.rs` adds an `ExpHistogramQuery` builder + `metrics().exp_histogram_quantile(q) -> Matrix`; `materialize_exp_quantile` reads the raw `scale` / `zero_count` / offsets / count-lists per row (Int32 + `List<UInt64>`) and calls the core fn — the same safe Rust-side materialization used for the merged explicit-histogram quantile, so no 7-arg DataFusion UDF is needed.

**Cross-point scale-aligned merge.** `ExpHistogramQuery::step(Duration)`: `materialize_exp_merged` groups points by (time-bucket, label set) and `exp_merged_quantile` **scale-aligns then sums**. `min_scale = min(scales)`; each point is down-scaled by `Δ = scale − min_scale`, mapping bucket index `i → i >> Δ` (arithmetic right shift = floor-divide by the width ratio, and correct for negative indices too: `-3 >> 1 = -2 = floor(-1.5)`). Positive/negative bucket maps and `zero_count` accumulate, then `densify` → `exp_histogram_quantile` at `min_scale`. The `i >> Δ` shift is exactly OTLP's scale-reduction rule; down-scaling to the **coarsest** scale in the group never invents resolution (up-scaling can't recover sub-bucket detail, so coarsest is the correct target).

### Summaries — `metrics_summary`

`imbh_core::SummaryRow` holds precomputed quantiles as index-paired `quantiles` / `values` `Vec<f64>` plus count / sum and identity fields. Summaries have **no OTLP temporality** (and no exemplars). `imbh_otlp::metrics_request_to_summary_rows` + `decode_metrics_to_summary_rows` handle `metric::Data::Summary`, mapping `ValueAtQuantile{quantile,value}` pairs to the two vectors. `summary_schema()` has two `List<Float64>` columns plus a **null `temporality` column** so all metric tables share the catalog identity; a separate `summary_buffer`, segments under `MetricsSummary`, and `write_summary_segment` complete the wiring. Facade registers `metrics_summary` as the **seventh** SQL table; `catalog()` reports kind `summary`. With every table materialized the `export` schema match became **total** — the "not exportable yet" arm was dropped (`Table` isn't `#[non_exhaustive]`), and an empty summary export yields a valid schema-only stream.

### Catalog and discovery

`metrics().catalog()` iterates the metric tables with a kind label per table (`(Table::MetricsHistogram, "histogram")`, `exponential_histogram`, `summary`, etc.). Because every metric table carries the same `metric` / `unit` / `temporality` identity columns — the summary table's always-null `temporality` column exists precisely to preserve this — the uniform `SELECT DISTINCT metric, unit, temporality` catalog query works unchanged across all kinds. `docs/PROMQL_TO_SQL.md` is a user-facing recipe doc translating common PromQL patterns (label selectors, time bucketing, `sum by`, `rate` / `increase`, `histogram_quantile`, instant vectors) to imbh SQL and typed `metrics()` calls, documenting the rate temporality split and the known v1 gaps.

### Rates

`imbh/metrics.rs` models rate as an internal `RateMode { Off, Delta, Counter }` composed onto the existing `metrics().range(...)` builder (`range_sql` is a clean three-way match):

- `MetricQuery::rate()` (Delta) — each `step` bucket value becomes `sum(value) / step_seconds` instead of `<agg>(value)`: the per-second rate of a **delta-temporality** sum (each OTLP data point is the increment since the last export). Example: `MetricQuery::sum("requests").rate().step(3s)`.
- `MetricQuery::rate_counter()` (Counter) — for a **cumulative monotonic** counter each `step` bucket value becomes `(max(value) − min(value)) / step_seconds`: the counter's in-bucket increase over the bucket width (assuming no reset within a bucket).

Two explicit methods rather than auto-detecting temporality per-series in SQL — the caller knows the metric's temporality, and branching the SQL per-row on the `temporality` column would be slower and murkier. `{step_seconds:?}` / `{step_ns:?}` renders a float literal so the divisor types as Float64. Cross-bucket boundary extrapolation (Prometheus-style) and multi-reset handling are out of scope for v1.

### Exemplars — metric→trace drill-down

OTLP exemplars link a sampled metric value to the trace that produced it. Coverage spans the four exemplar-bearing OTLP point types: gauge, sum, histogram, exp-histogram (summaries carry none). Each of `ScalarMetricRow` / `HistogramRow` / `ExpHistogramRow` has an `exemplars: String` field (canonical-JSON array; counted in `approx_bytes`). `imbh-otlp`'s `exemplars_json(&dp.exemplars)` encodes each exemplar as `{"time_unix_nano","value","trace_id"(hex),"span_id"(hex)}` with a nested `"attributes"` object (from `filtered_attributes` via `canonical_json_object(&kvs_to_pairs(...))`, included only when present). Non-finite values encode as JSON `null` (NaN/Inf aren't valid JSON numbers). trace_id/span_id are normalized with `fixed16` / `fixed8` so the hex is always 32 / 16 chars and round-trips through `from_hex`; empty exemplars serialize as `"[]"` (valid JSON), not `""`. Each metric schema + batch builder appends the `exemplars` (Utf8) column. Because metric WAL replay re-decodes the OTLP bytes and compaction is schema-agnostic, replayed rows get the field for free and it flows through segment merges — no migration risk.

Typed surface: `db.metrics().exemplars(metric) -> Vec<Exemplar>` unions the four exemplar-bearing tables (filtering `exemplars <> '[]'`) and parses each JSON array via `imbh-core`'s `parse_json` (no serde) into `Exemplar { time, value, trace_id, span_id, attributes }` (ids via `TraceId::from_hex` / `SpanId::from_hex`; attributes re-encoded canonical). The public `Exemplar` DTO is re-exported from the facade. Reuses opentelemetry-proto's `Exemplar` — no new deps (275 crates).

### Metrics-math hardening

The numerically-complex code (`imbh-core/histogram.rs` explicit + exponential quantile math, `imbh/metrics.rs` rate modes / merges / down-scaling) was verified correct under adversarial review; the fixes are input-hardening, exact for valid data:

- **`Aggregation::Count` runtime failure (Medium).** `RateMode::Off` emitted `count(value)` (Int64), but `materialize_matrix` downcasts the value column to `Float64Array`. Fix: emit `CAST(count(value) AS DOUBLE)` (no-op for min/max/avg/sum).
- **Metrics decode 4× → 1× (perf).** `ingest_otlp_metrics` and WAL replay decoded the same body four times; now decode once via `decode_metrics_request` and extract all four row kinds from the shared request.
- **exp-histogram merge OOM/overflow guard.** `densify` did `(max − min + 1) as usize` on i32 indices → overflow / multi-GB alloc. Now: i64 span + a `MAX_SPAN` cap (→ NaN), `saturating_add` on the index.
- **exp-histogram merge scale-delta shift overflow.** `delta = scale − min_scale` could overflow i32, and `>> delta` panics (debug) / masks (release) when `delta >= 32`. Now delta is computed in i64 and clamped to `[0,31]` (a >=32 down-scale correctly collapses buckets).
- **`exp_histogram_quantile` boundary index math → i64.** `offset + i as i32` could overflow on adversarial offsets; now i64 (exact, no behavior change for valid data).
- **`2f64.powi(-scale)` negation overflow.** `-scale` panics at `scale == i32::MIN`; switched to `2f64.powf(-(scale as f64))`.
- **exp-histogram NaN at extreme scale (Low).** At `scale = -10`, `bound(1) = 2^1024 = +inf`, so a bucket-start quantile computed `inf*0 = NaN`. A shared `interp()` falls back to the finite bucket edge when an edge overflows.
- **explicit `.step()` merge with mismatched bounds (Low).** Counts were summed element-wise even when a point's `explicit_bounds` differed from the group's first-seen bounds → silently-wrong quantile. Now the merge only sums when `bounds == entry.0`, skipping stray-bounds points (bounds are stable in practice; dropping beats corrupting).
- **unchecked `u64` count sums (Low).** Every count accumulation (both totals + both cumulative walks in histogram.rs, and the merge / exp-merge accumulations `*slot`, `pos` / `neg` entries, `zero` in metrics.rs) switched to `saturating_add`.
- **`Duration::as_nanos() as i64` truncation (Low).** A `Duration > ~292 years` truncated its low 64 bits → wrong bucketing. A `step_nanos()` helper (`i64::try_from(...).unwrap_or(i64::MAX).max(1)`) is used at all three step sites.
- **span-duration i64 overflow, 2 sites.** `start + duration_ns as i64` in `assemble_trace` and `write_spans_segment` could overflow → wrong trace duration / weakened segment `max_time` pruning. Both now `saturating_add`.
- **SQL escaping verified clean.** `esc()` doubles single quotes (correct for DataFusion's default GenericDialect; backslash is literal) at every site including the `json_get_str(attributes,'<key>')` key path; table names are never user input. `attr_in` empty set = `1 = 0`; `attr_not_in` uses `(g IS NULL OR g NOT IN (list))` to keep key-less rows.

## Files

- `crates/imbh-core/` — `SpanRow`, `ScalarMetricRow`, `HistogramRow`, `ExpHistogramRow`, `SummaryRow`, `Exemplar`; `histogram` module (`histogram_quantile`, `exp_histogram_quantile`, `interp`, `densify`); `parse_json`; `TraceId::from_hex` / `SpanId::from_hex`.
- `crates/imbh-otlp/` — `decode_traces_to_rows` / `traces_request_to_rows`; `decode_metrics_request`; `decode_metrics_to_rows` / `metrics_request_to_rows` (scalar); `metrics_request_to_histogram_rows` / `decode_metrics_to_histogram_rows` / `histogram_row`; `metrics_request_to_exp_histogram_rows` / `decode_metrics_to_exp_histogram_rows` / `exp_histogram_row`; `metrics_request_to_summary_rows` / `decode_metrics_to_summary_rows`; shared helpers `kvs_to_pairs` / `scope_to_json` / `service_name` / `canonical_json_object` / `temporality_str` / `nonzero` / `exemplars_json` / `fixed16` / `fixed8`.
- `crates/imbh-storage/` — `metric_buffers` / `metric_segments` maps; `histogram_buffer` / `exp_histogram_buffer` / `summary_buffer`; `metric_scalar_schema` / `histogram_schema` / `exp_histogram_schema` / `summary_schema`; `*_rows_to_batch` builders; `write_*_segment`; `replay_*`; `schema_*`; `buffer_snapshot_*`; `segments_*` / `segment_paths_*`; `table_from_manifest_name`; `SCALAR_METRIC_TABLES`.
- `crates/imbh-query/` — `histogram_quantile` `ScalarUDFImpl` registered in `session_context` (delegates to `imbh_core::histogram_quantile`); `columnar_to_array`; generic `coerce` cast.
- `crates/imbh/metrics.rs` — `MetricQuery` (`RateMode { Off, Delta, Counter }`, `.rate()` / `.rate_counter()`); `HistogramQuery` (`.quantile` / `.group_by` / `.filter` / `.range` / `.since` / `.step`, `merged_sql`, `materialize_merged_quantile`); `ExpHistogramQuery` (`.step`, `materialize_exp_quantile`, `materialize_exp_merged`, `exp_merged_quantile`); `metrics().histogram_quantile` / `exp_histogram_quantile` / `catalog` / `series` / `exemplars`; `materialize_matrix`; `step_nanos()`.
- Facade — `Db::ingest_otlp_metrics` / `try_ingest_otlp_metrics`, `ingest_metrics_inner`; `Query::collect` table registration; `SIGNAL_METRICS` replay dispatch; re-exported `Exemplar` DTO.
- `docs/PROMQL_TO_SQL.md` — PromQL→SQL / typed-builder recipe doc.

## Test Coverage

- Spans / scalar decode: `normalizes_histograms` / `normalizes_exp_histograms` / `normalizes_summaries` each also assert the other extractors ignore their point type; decode-from-bytes paths.
- Round-trips (ingest → buffer query → seal → segment query → reopen with one segment-recovered + one WAL-replayed): `metrics_ingest_seal_query_recover`, `histogram_table_query_and_roundtrip`, `exp_histogram_table_query_and_roundtrip`, `summary_table_query_and_roundtrip`.
- Quantile math: `histogram_quantile_interpolates` (p0/p10/p50/p99/p100 hand-checked against `bounds=[1,5] counts=[2,3,2]`, degenerate phi, monotonicity); exp-histogram unit test (positive/negative/zero/degenerate); `typed_histogram_quantile` (p50=3.0, default p95→5.0 overflow clamp); `typed_exp_histogram_quantile` (scale-0 bucket `(1,2]`×4 → p50=1.5).
- Merges: `histogram_quantile_merges_across_step` (`[10,0,0]`+`[0,0,10]` → merged p50=1.0 on `counts=[10,0,10]`); `exp_histogram_quantile_merges_across_scales` (scale-1 offset-2 `[8]` + scale-0 offset-0 `[2]` → merged `{0:2,1:8}`, p50=2.75).
- Rates: `metrics_rate_of_delta_sum` (3 deltas of 3 in a 3s bucket → 3.0 req/s; raw sum 9); `metrics_rate_of_cumulative_counter` (10→13→16 → `(16-10)/3 = 2.0/s`); shared `otlp_sum(service, metric, temporality, points)` builder.
- Hardening: `metrics_count_aggregation_returns_float`, `exp_histogram_quantile_extreme_scale_stays_finite`, `exp_histogram_merge_guards_pathological_offsets`, `exp_histogram_merge_guards_extreme_scale_delta`.
- Discovery / exemplars: `metrics_typed_api` (catalog reports histogram); `metrics_exemplars_round_trip` (trace-linked exemplar on a scalar and a histogram table; `"attributes":{"sampler":"always_on"}` round-trips; no-exemplar point stores `"[]"`).

## Pitfalls

- **Temporality drives rate.** Applying `.rate()` (delta semantics, `sum/step`) to a **cumulative** sum overcounts (it sums absolute counter readings); use `.rate_counter()` for cumulative monotonic counters. There is no auto-detection — the caller must pick the method matching the metric's temporality.
- **Counter resets.** `rate_counter`'s `(max−min)/step` assumes no reset within a bucket; cross-bucket extrapolation and multi-reset handling are v1 gaps.
- **Scale alignment (exp histograms).** Merging exp-histograms across points requires aligning scales by down-scaling the finer one to the group's **coarsest** scale (`i >> Δ`, an arithmetic right shift for correct floor-division on negative indices); up-scaling can't recover sub-bucket detail. `.step()` is required to merge — per-data-point quantiles cannot simply be averaged.
- **List-column child field.** Declare List children with `Field::new_list_field(dt, true)` to match `ListBuilder`'s default (`item`, nullable); `Field::new("item", dt, false)` mismatches and `RecordBatch::try_new` fails.
- **OTLP bucket_counts are per-bucket**, not cumulative like Prometheus `le` series — quantile walks must accumulate them.
- **Non-finite floats aren't JSON numbers.** Exemplar `NaN`/`Inf` values encode as JSON `null`.
- **Overflow surfaces on adversarial telemetry.** i32 bucket-index / offset / scale-delta math, `u64` count sums, and `Duration→i64` nanos can overflow on malformed OTLP; the code uses i64 promotion, `saturating_add`, clamps (`[0,31]`), caps (`MAX_SPAN` → NaN), and finite-edge `interp()` fallback. These are exact for valid data and only degrade (NaN / collapsed bucket) on malformed input.
- **Delta→cumulative accumulation at ingest is still deferred** — the one stateful piece, keyed by series identity and checkpointed against the WAL watermark; storage stores temporality verbatim for now.
