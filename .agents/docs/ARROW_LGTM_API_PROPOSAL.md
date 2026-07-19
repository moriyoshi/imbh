# Proposal: Arrow-native result surface for the imbh-lgtm query API

Status: Phase 1 landed (2026-07-22). Phase 2 (FFI) deferred to the binding side, not this
crate. Phase 3 (eval-in-engine) deferred pending a real driver - see the decision note below.
Date: 2026-07-22.

## Context

The `imbh-lgtm` compatibility layer (PromQL / LogQL / TraceQL) currently returns
Rust-native composite types from its executors and `*SemanticsExt` traits:

- `execute_promql(...) -> Vec<PromSeries>` where `PromSeries { labels: LabelSet, samples: Vec<FloatSample> }`
- `execute_logql(...) -> Vec<LogSeries>` (same shape as `PromSeries`)
- `execute_traceql(...) -> Vec<TraceQueryMatch>` where `TraceQueryMatch { trace_id: String, spanset: TraceSpanset { selected_span_ids: Vec<String> } }`

These are the *evaluation outputs* of the query-language layer, computed in Rust
*above* the Arrow SQL engine, not raw scan results. The underlying fetch already
flows out of DataFusion as `RecordBatch`, is materialized into owned Rust rows
(`MetricPoint` / `LogEntry` / `Trace`) by the `imbh` facade, then re-shaped by the
`imbh-lgtm` source adapters and reduced by the executors into the `Vec<Struct>`
results above.

The goal of this change is to add an **Arrow-native, view-first result surface**
for the LGTM layer that serves four motivations the user has confirmed:

1. Out-of-process / FFI bindings (zero-copy `RecordBatch` over the Arrow C Data Interface), mirroring the facade's existing `proto`/`cdata` `*_batches` pattern.
2. Letting in-process callers run further DataFusion SQL / DataFrame analytics on results.
3. Pushing evaluation itself into the DataFusion engine where the semantics permit.
4. Consistency and reduced allocation: stop round-tripping through owned Rust structs.

Hard constraint from the user: **use views (buffer-sharing / `*View` arrays) as
much as possible, resorting to `unsafe` where needed, as long as lifetimes are
assured**. The copy-the-`Vec<Struct>` shim is therefore explicitly rejected as the
foundation; the design targets buffer reuse end-to-end.

The API is added **alongside** the existing typed `Vec<...>` returns (non-breaking).
`imbh-tui` is the only in-process consumer today and keeps its current typed path.

## Where views actually help (and where they cannot)

A view shares an existing Arrow buffer instead of copying it. That only pays off
for data that survives *unchanged* from input to output:

- Labels / stream labels (PromQL, LogQL) - string payload, carried through grouping. **View candidate.**
- `trace_id` and `selected_span_ids` (TraceQL) - taken verbatim from the scan. **Prime view candidates.**
- Sample timestamps and values (PromQL, LogQL) - **freshly computed** by rate / count-over-time / grouping. These are new numbers, not views of anything; they are always materialized. This is inherent, not a limitation to fix.

So the zero-copy win concentrates on the **string columns** (labels, ids). The
numeric sample columns are small (`i64` / `f64`) and derived, so copying them is
negligible and unavoidable.

### FFI-safety of views: owned buffers vs mmap

Arrow buffers are `Arc`-refcounted. A `RecordBatch` whose string columns are
`Utf8View` arrays holding `Arc<Buffer>` references is **self-contained**: it can
cross the C Data Interface without a keep-alive token, satisfying the facade's
documented FFI invariant. The DataFusion Parquet reader decodes into owned heap
Arrow buffers, so sharing those `Arc<Buffer>`s downstream is safe with no `unsafe`.

`unsafe` is only required if we alias memory whose lifetime is **not** captured by
an `Arc` - e.g. borrowing directly from an mmap'd segment we then drop. If we ever
take that path, the returned batch must own a keep-alive handle to the segment, and
that is the only place `unsafe` view construction is justified. Default path needs
no `unsafe`; the `Arc<Buffer>` sharing model already expresses the views.

## Proposed canonical Arrow schemas

Timestamps use `Timestamp(Nanosecond, None)` for SQL-friendliness (confirm against
the facade's existing metric/log batch schema during implementation; fall back to
`Int64` ns if the facade batches use that, to keep one convention). String columns
use `Utf8View` so they can be built from shared buffers. Labels use a
`Map<Utf8View, Utf8View>` column.

Schema shape is **long form** (one row per sample) - decided, because it is
directly `GROUP BY`/join-queryable, which is motivation 2. The alternative
nested/wide form (List columns per series) was rejected as it forces `UNNEST`
before any SQL analytics.

### Zero-copy refinement (buffer provenance)

True scan-to-output buffer sharing is bounded by where data survives evaluation
unchanged. Two facts sharpen this:

- The `imbh` facade hands the LGTM layer **owned** rows (`MetricPoint` / `LogEntry` / `Trace`), already copied out of the scan `RecordBatch` by the facade materializers. So the deepest zero-copy path requires the LGTM adapters to consume the facade's `*_batches` (`RecordBatch`) surface directly instead of the owned-struct APIs - a Phase 1b/3 refactor of `source.rs`.
- The executors **reconstruct** label sets during `sum by (...)` grouping (`BTreeMap<LabelSet, ...>`), so aggregated-query result labels are genuinely new; only bare-selector labels and TraceQL `trace_id`/`span_ids` pass through verbatim.

Consequence for sequencing: Phase 1a emits the **view-capable output type**
(`Utf8View`/`Map<Utf8View,Utf8View>`) with the schema locked, but still copies at
the `Vec<Struct>` -> batch boundary. Because the *schema* is already the view type,
Phase 1b removes the copy (share `Arc<Buffer>` from the facade `*_batches`) **without
any API change**. This is the correct order: land the surface + schema + differential
tests first, then swap the buffer source underneath.

### Metric series (PromQL `execute_promql_batches`, LogQL `execute_logql_batches`)

Long form, one row per sample, sorted by `(labels, ts)`:

| column | type | notes |
| --- | --- | --- |
| `labels` | `Map<Utf8View, Utf8View>` non-null | view into scan label buffers |
| `ts` | `Timestamp(Nanosecond, None)` non-null | computed step timestamp |
| `value` | `Float64` non-null | computed; NaN / +/-Inf survive natively in Arrow |

### PromQL histograms (`PromHistogramSeries`)

| column | type |
| --- | --- |
| `labels` | `Map<Utf8View, Utf8View>` |
| `ts` | `Timestamp(Nanosecond, None)` |
| `explicit_bounds` | `List<Float64>` |
| `bucket_counts` | `List<UInt64>` |

### Trace matches (TraceQL `execute_traceql_batches`)

One row per matched trace:

| column | type | notes |
| --- | --- | --- |
| `trace_id` | `Utf8View` non-null | view into scan buffer |
| `span_ids` | `List<Utf8View>` non-null | selected span ids, viewed |

Empty results still carry the schema (mirrors `run_sql` / `collect_with_schema`).

## Phasing (sequenced by risk)

### Phase 1 - Arrow `*_batches` surface + view dedup (LANDED 2026-07-22)

- New `crates/imbh-lgtm/src/batch.rs` (feature = `source`) with the schema constructors above and long-form builders. Arrow types come via `imbh::arrow` (the re-exported `datafusion::arrow`, single pinned instance) - no new dependency.
- `execute_promql_batches` / `execute_logql_batches` / `execute_traceql_batches` added to the three `*SemanticsExt` traits, returning `RecordBatch` alongside the untouched typed `Vec<...>` returns. Public builders + `*_schema()` accessors re-exported.
- **View sharing landed as string-view dedup**, not scan-buffer aliasing: every `Utf8View` column is built with `StringViewBuilder::with_deduplicate_strings()`, so a series' per-sample-repeated labels (and shared label vocabulary) collapse to one buffer copy per distinct string > 12 bytes. Full scan-buffer aliasing was found infeasible without either changing the public `LabelSet` type or a parallel evaluator - see the decision note.
- Tests: long-form shape + Map/`Utf8View` labels, trace_id/span_ids, empty-carries-schema, and a buffer-sharing assertion (200 samples of a 24-byte label -> one ~24-byte buffer copy). Gate green on `imbh-lgtm --features source`.

### Phase 1b update (2026-07-22): blockers removed, payoff still marginal

The two conditions Phase 1 cited as making full scan-buffer aliasing infeasible - "changing the
public `LabelSet` type" and "a parallel evaluator" - **have both since landed** (the `Cow`-based
`LabelSet<'a>` + `self_cell` borrowed evaluator, and the LogQL Level-2 `query_batches`/
`StreamLabelReader` adapter that consumes the facade's batch surface directly; see
`LABELSET_ARROW_REFACTOR.md` / JOURNAL). So Phase 1b is now **unblocked**.

It is still **not worth building**, for reasons Phase 1's own analysis already gave and the refactor
confirms:
- The dedup `StringViewBuilder` already collapses a series' per-sample-repeated labels to **one copy
  per distinct string** - the practical win. Aliasing only removes that last one-copy-per-distinct,
  and the label vocabulary is small.
- Aggregating queries (`sum by`, `count_over_time` grouping) **reconstruct** their result labels, so
  those are genuinely new strings, not views - only bare selectors and TraceQL `trace_id`/`span_ids`
  pass through verbatim.
- The `*_batches` pipeline's dominant materialization is the **input** side (facade DTO / attribute
  parse), addressed by the input-side Level-2 work, not the output copy.

Tractable-but-modest slice the refactor does enable: the `execute_*_batches` paths currently
`into_owned` the result labels and then copy them into the builder; since they return a `RecordBatch`
(not owned series), that `into_owned` is pure waste and could be skipped by building the batch from the
borrowed working set. Bounded to result cardinality (M series), so small. Revisit only with a driver.

### Phase 2 - FFI / zero-copy binding surface (deferred, binding-side)

Decision (2026-07-22): the Arrow C Data Interface handoff belongs on the **binding side**
(the Go / cgo / `arrow-go` project), consuming the `RecordBatch` this surface already
returns. It is not `imbh-lgtm`'s responsibility, so nothing is added here. The `*_batches`
methods are the seam the binding builds on.

### Phase 3 - Push evaluation into DataFusion (deferred, revisit later)

Investigated for LogQL `count_over_time`/`rate` (2026-07-22) and deferred - the ROI is poor
today:

- **No zero-copy gain.** Labels are stored as canonical-JSON blobs (only `service` is a real column); SQL grouping via `json_get_str(attributes,'k')` produces freshly-allocated strings, not views. So "eval in engine" buys no buffer sharing over Phase 1.
- **Likely a perf regression.** The reference evaluator uses *sliding, overlapping* windows (independent window per instant). A faithful SQL form needs a generated instants table **range-joined** on `ts > at-offset-window AND ts <= at-offset` - an inequality join far slower than the Rust sliding scan. The `volume_by` tumbling-bucket helper is equivalent only in the degenerate `step == window, offset == 0` case.
- **Three divergence hazards** requiring careful differential testing: overlapping-window multi-counting, left-open/right-closed vs half-open boundaries, and NULL-vs-absent label identity (`log_labels` drops absent labels; naive `GROUP BY` puts NULL in its own group and keeps the key).

PromQL `rate()` is strictly worse (counter-reset correction + Prometheus extrapolation as a
UDAF). TraceQL structural matching stays in Rust regardless (graph traversal, not relational).
Revisit only with a concrete driver (e.g. a query too large for the bounded in-memory working
set), and only where output can be proven equal to the reference evaluator via the existing
test corpus.

## Critical files

- `crates/imbh-lgtm/src/model/promql.rs` - `PromSeries`, `execute_prom`, `bounded_series` accumulator.
- `crates/imbh-lgtm/src/model/logql.rs` - `LogSeries`, `eval_log_range_reference` accumulator.
- `crates/imbh-lgtm/src/model/traceql.rs` - `TraceQueryMatch`, `TraceSpanset`, `execute_traceql`.
- `crates/imbh-lgtm/src/source.rs` - `*SemanticsExt` traits + `imbh` impls; `metric_labels` / `log_labels` / `semantic_trace` (where owned strings are built today and where buffers must instead be retained for views).
- `crates/imbh/src/lib.rs` - `pub use datafusion::arrow`, `SendableRecordBatchStream`, `cdata` FFI re-exports, and the `Query::collect_with_schema` / batch-ownership invariant to mirror.
- `crates/imbh/src/{logs,traces,metrics}.rs` - existing `*_batches` methods and `downcast` / `get_str` column helpers to reuse.
- `crates/imbh-tui/src/lib.rs` - unchanged (keeps the typed `Vec<...>` path).

## Verification (as landed)

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p imbh-lgtm --features source` all green (51 tests). `imbh-tui` library builds clean, confirming the new `*_batches` methods are additive.
- `batch.rs` unit tests cover: long-form one-row-per-sample + Map/`Utf8View` labels, trace_id + span_ids list, empty-carries-schema, and the buffer-sharing dedup assertion.
- Footprint: `cargo tree -p imbh-lgtm --features source` shows only the existing `datafusion -> arrow 58.3.0` subtree - no new dependency, and the default (feature-off) build stays free of it.

Known unrelated breakage: a concurrent change to `crates/imbh-tui/src/lib.rs` (`mascot_frame` / `MASCOT_FRAMES_ASCII`, +155 lines, not part of this work) breaks the `imbh-tui` *test module* only; left untouched per the no-touching-concurrent-work rule.
