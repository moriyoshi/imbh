# imbh-lgtm: LGTM query languages and Arrow-native reads

## Summary

`imbh-lgtm` is the crate implementing the Grafana LGTM stack's query languages against imbh: LogQL (Loki), TraceQL (Tempo), and PromQL (Prometheus/Mimir), plus the native-IMBH source adapter that executes them. It layers bounded reference evaluators, dialect parsers/translators, an Arrow-native `*_batches` result surface, a borrowed-read (`Cow` + `self_cell`) evaluator core, and Level-2 raw-Arrow input reads that skip DTO materialization for LogQL and metrics.

## Key Facts

- Crate `imbh-lgtm` is a consolidation of the former `imbh-semantics` (models + reference evaluators) and `imbh-query-language` (parsers/translators) into one crate: modules `model`, `syntax`, plus `source.rs` behind the `source` feature.
- Dependency direction preserved: `imbh-lgtm` default has 0 `imbh`-facade edges (parse/evaluate-only stays light); `--features source` has 1 edge; the `imbh` facade has 0 `imbh-lgtm` edges. Footprint isolation lives on the `source` feature flag, not on a crate boundary.
- Three compatibility profiles are exact capability ids, not vague claims: `imbh.promql.p1.v1`, `imbh.logql.l1.v1`, `imbh.traceql.t1.v1`. Unsupported constructs fail translation; no approximate fallback.
- No semantic adapter builds SQL. Metric names, label keys/values, regexes, log text, attribute keys, and time bounds all flow through the facade's `SqlParams` as bound parameters.
- IMBH LogQL dialect adds `|?` / `!?`: tokenized term-match (Tantivy-accelerated) distinct from LogQL standard `|=`/`!=` (substring, unindexed) and `|~`/`!~` (regex, unindexed).
- Arrow result surface: `execute_promql_batches` / `execute_logql_batches` / `execute_traceql_batches` return `RecordBatch`es in long form; landed with `Utf8View` string-view dedup. Phase 3 (eval-in-DataFusion) deferred.
- Borrowed-read core: `LabelSet<'a> = Box<[(Cow<'a,str>, Cow<'a,str>)]>` plus `self_cell` packs across all three paths. One new dep total: `self_cell` 1.3 (macro-only, zero transitive deps).
- Level-2 (raw-Arrow input) reads wired into LogQL (`query_batches` + `StreamLabelReader`) and metrics (`points_batches` + `metric_labels_from_batch`); trace Level-2 measured and declined.
- Upstream boundary fidelity: PromQL lookback is left-open `(at-lookback, at]`; TraceQL negated matchers (`!=`/`!~`) do **not** match a span missing the attribute (Tempo semantics, not three-valued logic).

## Details

### Bounded language semantics and translators

The PromQL/LogQL/TraceQL compatibility profiles are implemented as separate semantics, syntax, and presentation layers, because the native IMBH query builders are endpoint-shaped conveniences, not drop-in language evaluators.

The evaluator does not require all retained data. Reference functions accept a fully materialized *bounded working set* so semantic tests stay small and deterministic. Production calls first derive `FetchBounds` from evaluation range, lookback/window/offset, and limits, then ask a signal-specific source for only that interval. Facade sources push those bounds and limits into IMBH queries.

Profiles are capability ids that pin exact implemented subsets and reference versions. Unsupported constructs fail translation; there is no approximate fallback to native bucket APIs or SQL. Prometheus metric names require explicit `TranslateContext` resolution so a query cannot guess temporality or confuse an OTLP histogram name with a classic `_bucket` series.

Histogram order is semantic. Cumulative explicit OTLP bucket counts are converted to classic cumulative buckets; reset-aware rates are computed per source series and per bucket before aggregation; only then is `histogram_quantile` evaluated. Applying aggregation before rate would hide resets; applying quantile before aggregation would produce a different statistic.

TraceQL is deliberately two-phase: candidate trace ids first, then complete traces, because structural operators are incorrect over partial traces.

### Native semantic query models and adapter ownership

The `imbh` facade does not depend on or re-export the semantics crate. It exposes only the native capabilities the language source contracts require: bounded raw scalar/histogram point queries, exact log string predicates, inclusive ranges, match-none plans, and assembled-trace-start candidate bounds. The raw metric timestamp is explicitly aliased because DataFusion otherwise gives the projected cast and the hidden `ORDER BY time` expression the same name.

The optional `source` feature owns all mappings from semantic fetch requests to native models, the three source implementations, and the extension execution traits used by hosts (e.g. the TUI). This keeps parser-only semantics dependency-light and makes the dependency point toward the facade only when native execution is requested.

No semantic adapter builds SQL. User strings flow through `SqlParams` as bound params. Injection-shaped execution regressions cover metrics and logs; native compiler tests assert that user strings never appear in emitted SQL. Prometheus missing-label behavior is preserved with empty-string coalescing, regex matchers are anchored, all `__name__` matchers are conjunctive, fetch end bounds are inclusive, and TraceQL hydrates complete traces after bounded trace-start candidate selection.

TraceQL operator precedence is layered (`||` < `&&` < structural, each left-associative), matching Tempo. A flat single-precedence fold previously mis-parsed `{a} || {b} && {c}` as `({a} || {b}) && {c}`; the paren-nesting depth guard is preserved.

### Upstream-fidelity boundary semantics (PromQL lookback, TraceQL missing attributes)

Two reference-evaluator divergences from upstream were found by reading the upstream rule rather than by a failing test — in both cases the *existing tests enshrined the wrong behaviour* and had to be reconciled along with the fix. Both are boundary/absence cases, which is where bounded reference evaluators drift from their originals.

- **PromQL lookback is left-open, `(at-lookback, at]`.** `select_instant` (`crates/imbh-lgtm/src/model/promql.rs`) used a closed lower bound (`timestamp_ns >= earliest`). Prometheus's `vectorSelectorSingle` drops a point when `t <= refTime - lookbackDelta`, i.e. the lower bound is exclusive. Changed to `> earliest`, which also aligns it with the sibling `select_range` (already open). Two tests that encoded the closed bound (`selectors_apply_lookback_and_open_range_boundary`, `expression_uses_the_requested_lookback`) were reconciled and the doc comment updated.
- **TraceQL negated matchers do NOT match a span missing the attribute.** Tempo semantics are *not* SQL/PromQL three-valued logic: a span lacking the referenced attribute is not matched by **any** condition, negated ones included — the only presence test is the explicit `nil` literal. The evaluator (`model/traceql.rs`) previously let `!=` / `!~` on an absent attribute evaluate true. `compare_value` now short-circuits `actual == None` against a non-`nil` expected value to not-matched. `{ .foo = nil }` / `{ .foo != nil }` presence semantics are preserved, and a regression test pins the absent-attribute case.

Contrast this with the *native* IMBH matcher vocabulary, where `attr_not_in` deliberately **is** NULL-aware and keeps rows lacking the key, and PromQL label negation keeps label-absent series (both in `query-engine-and-typed-apis.md`). Absence semantics are per-language; do not carry one language's rule into another.

### Consolidation of imbh-semantics + imbh-query-language into imbh-lgtm

The two query-language crates were merged into one, `imbh-lgtm`, named for the stack whose query surfaces it targets: Loki (LogQL), Tempo (TraceQL), Prometheus/Mimir (PromQL) — the Grafana LGTM stack. The prior generic names implied an ecosystem-neutral query layer this code is not.

- The split was collapsible to modules because the dependency edge was one-way and shallow. `imbh-query-language` depended on `imbh-semantics` only for AST/model types; no cycle. The merge is: `model` (expression types + reference evaluators, was `imbh-semantics`), `syntax` (parsers/translators, was `imbh-query-language`), and `source.rs` (the native-IMBH adapter, behind the renamed `source` feature, was `imbh-source` → `dep:imbh`). Layer separation is preserved as a module boundary.
- Near-zero content edits were needed because everything resolves through crate-root re-exports. Model/source files reference sibling types exclusively as `crate::X` (never `crate::module::X`), so once `lib.rs` re-exports the full public surface flat at the crate root, they compile verbatim from their new `model/` subdir. Only the three parser files needed edits (`imbh_semantics::` → `crate::`, `crate::parser::` → `super::parser::`). Public API and behavior are unchanged.
- Footprint isolation moved intact from a crate boundary to a feature flag. The heavy dependency (the `imbh` facade → DataFusion/Tantivy) was already gated by the `imbh-source` feature *inside* `imbh-semantics`, so merging is footprint-neutral. Post-merge: `imbh-lgtm` default has 0 `imbh` edges, `--features source` has 1, the `imbh` facade has 0 `imbh-lgtm` edges. Tests: 38 default, 41 with `source`.
- Lesson: a workspace-wide crate merge can surface a latent `cfg`-gated bug elsewhere via feature unification. After the graph change, `cargo test --workspace` failed to compile `imbh-query` on a pre-existing `search`-gated defect the new unified feature set exercised. After a dependency-graph edit, a `--workspace` failure may live in a crate you never touched — check whether feature unification, not your edit, is the trigger.

Naming note: the "G" (Grafana) is a dashboard UI with no query language, so `imbh-lgtm` names the stack while the crate implements the L/T/M query languages. `LGTM` also reads as "looks good to me", but the stack reading dominates.

### IMBH LogQL dialect with the Tantivy `|?` operator

The Logs search box spans five layers (parser → model → semantics bridge → native `LogQuery` SQL → host), plus a Tantivy pushdown for the new operator. The LogQL parser was made to accept a bare selector (e.g. `{service="api"} |= "timeout"`) rather than requiring a leading aggregation function.

Key semantic finding: IMBH's `matches()` UDF is **tokenized term-AND**, not substring — proven by `full_text_matches_in_sql` (`matches("err") == 0`, "substring is not a term match"). LogQL's `|=` is exact substring. They genuinely differ for partial-token needles, and only the term form can use the `.tidx` index. Rather than silently redefine `|=`, the design keeps standard LogQL semantics and adds a **dialect operator `|?`** (and negation `!?`) for accelerated term search. So: `|=`/`!=` stay substring (`strpos`, unindexed), `|~`/`!~` stay regex (unindexed), `|?`/`!?` are tokenized term match (Tantivy-accelerated).

Per layer:
- **Model** (`imbh-lgtm/model/logql.rs`): `LogFilter::LineMatches`/`LineNotMatches`. Their reference evaluator calls `imbh_core::matches_terms` — the same shared tokenizer `imbh-index` wraps into its Tantivy analyzer, so the in-memory re-filter (on the `rate({} |? "x" [5m])` metric path inside `eval_log_range_reference`) agrees with the index byte-for-byte. This makes the `imbh-core` dep a correctness requirement, not just DRY (adds `tokio` rt to the parse-only profile, but not the DataFusion/Tantivy subtree the `source` feature gates).
- **Parser** (`syntax/logql.rs`): a top-level branch — a query starting with `{`, `|`, or `!` is a bare log selector (`ImbhQueryModel::LogSelector(LogFilter)`); everything else is the existing metric path. `parse_log_range` made the `{...}` selector optional so a bare `|? "timeout"` search means implicit match-all. Added `|?`/`!?` to the line-filter loop.
- **Native SQL** (`imbh/logs.rs`): `StringPredicate::Matches`/`NotMatches` render `matches(field, ?)` / `NOT matches(field, ?)`. Subtlety: other string predicates wrap the field in `coalesce(f,'')`, but the provider's index pushdown (`matches_text_terms`) only claims `matches(<bare column>, ?)`, so the term ops must render against the **bare** `field` to stay index-eligible.
- **Bridge** (`imbh-lgtm/source.rs`): `apply_log_filter` maps the two new filters to the two new predicates. The host reuses the `pub` `build_log_query` to turn an extracted `LogFilter` into a native `LogQuery`, then overrides direction/limit to restore most-recent-first paging (the bridge defaults to ascending + one-over for its own paging).

Negation pushdown optimization: positive `|?` chains already collapse to one `search_body` BooleanQuery (the provider combines every `matches(body, …)` conjunct at `provider.rs:219`). The gap was negation: `NOT matches(body, ?)` was `Unsupported` for pushdown (`matches_text_terms` returns None for the `Expr::Not` wrapper), so `!?` fell back to a DataFusion scan. Closed with `imbh_index::search_body_bool(dir, must, must_not)`, which builds one `BooleanQuery` with `Occur::Must`/`MustNot` (a match-all base when `must` is empty, so a pure-negation query subtracts from every row). The provider recognizes `NOT matches` conjuncts, threads `not_terms` through the partition/iter, and calls `search_body_bool`. A pure `|?`/`!?` chain is now one index query with no residual body scan. Pushdown stays `Inexact`, so the `FilterExec` above re-checks — the index remains a pure accelerator.

Cost-gate gotcha: `SELECTIVITY_THRESHOLD = 0.5` with a `>=` gate, so a term hitting exactly half the rows (3/6) gates to a full scan (`None`), not a RowSelection. With single-word fixture bodies no row carries two body terms, so `+X -Y` only subtracts under contradiction.

### Arrow-native result surface (Phase 1a / 1b; Phase 3 deferred)

The executors and `*SemanticsExt` traits originally returned only Rust-native composite types (`Vec<PromSeries>` / `Vec<LogSeries>` / `Vec<TraceQueryMatch>`), hiding the Arrow advantage the facade already exposes (`Query::collect -> Vec<RecordBatch>`, `stream`, `export`, the `proto`/`cdata` `*_batches` binding path). Design + rationale: `.agents/docs/ARROW_LGTM_API_PROPOSAL.md` (four confirmed motivations: FFI/zero-copy bindings, SQL/analytics on results, pushing eval into the engine, consistency/less allocation; a parallel surface, not a replacement).

Key reframing: these three result types are the *evaluation outputs* of PromQL/LogQL/TraceQL, computed in Rust *above* the SQL engine, not raw scan rows. So Arrow "views" only help where data passes through unchanged — labels on bare selectors, and TraceQL `trace_id`/`span_ids`. Sample timestamps/values are freshly computed by rate/window/group and always materialized; `sum by (...)` grouping *reconstructs* label sets, so aggregated-result labels are genuinely new.

Phase 1a: `crates/imbh-lgtm/src/batch.rs` (feature = `source`) with canonical long-form schemas and builders. Layout is **long form** (one row per sample), `labels: Map<Utf8View,Utf8View>`, `ts: Timestamp(ns)`, `value: f64`; TraceQL as `{ trace_id: Utf8View, span_ids: List<Utf8View> }`. Schemas are derived from the built arrays' own data types so an empty result carries a schema identical to a populated one. Added `execute_promql_batches` / `execute_logql_batches` / `execute_traceql_batches` to the three traits alongside the typed returns (typed API and hosts untouched). Arrow types reached via `imbh::arrow` (the re-exported `datafusion::arrow`, single pinned instance, `arrow 58.3.0`) — no new dependency, no footprint change; default feature-off build stays free of it.

Phase 1b (string-view dedup): full scan-buffer sharing is not cheaply reachable for two structural reasons — (1) the facade exposes no batch-returning entry point for the queries `imbh-lgtm` runs (`MetricsApi::points` returns owned `Vec<MetricPoint>`; only `logs().query_batches`, `metrics().range_batches`, `traces().span_metrics_batches` exist); (2) the evaluator consumes `Vec<PromSeries>` whose public `LabelSet` owns its `String`s, so views can't flow through without changing that public type or standing up a parallel batch-native evaluator. Upside is bounded anyway (only bare-selector labels and TraceQL `trace_id`/`span_ids` are ever pure passthrough). Landed the safe, bounded win: `batch.rs` builds every `Utf8View` column with `StringViewBuilder::with_deduplicate_strings()` (`view_builder()` helper, used for labels, `trace_id`, `span_ids`). In long form a series' labels repeat once per sample, so dedup collapses that to one buffer copy per distinct string > 12 bytes (short strings inline into the view regardless). Test `repeated_labels_share_one_buffer_copy_across_samples`: 200 samples of a 24-byte label value hold a single ~24-byte buffer copy, not 4800. No unsafe, no public-type change. `arrow` also exposes `append_view_unchecked` for a future manual buffer-sharing path.

Phase 3 (push eval into DataFusion) deferred. FFI/C Data Interface (Phase 2) is a binding-side concern — the consuming project takes the `RecordBatch` this surface already returns; nothing to add in `imbh-lgtm`. Phase 3 was declined (LogQL feasibility probe): (1) no zero-copy gain — LogQL labels are canonical-JSON blobs (only `service` is a real column), so SQL grouping via `json_get_str` yields fresh strings; (2) likely perf regression — the reference evaluator uses sliding, overlapping windows, so a faithful SQL form is an instants-table *range-join* (`ts > at-offset-window AND ts <= at-offset`), an inequality join slower than the Rust sliding scan; the `volume_by` tumbling helper matches only when `step == window && offset == 0`; (3) three silent-divergence hazards (overlapping-window multi-count, left-open/right-closed vs half-open bounds, NULL-vs-absent label identity since `log_labels` drops absent labels). PromQL rate is strictly worse (counter-reset + extrapolation UDAF); TraceQL structural matching is graph traversal, stays in Rust. Feasible for LogQL count/rate and PromQL rate-as-UDAF with the Rust evaluators as the differential oracle. Revisit only with a concrete driver (working set too large for bounded in-memory eval) and reference-differential tests.

### Borrowed-read refactor: Cow-based LabelSet + self_cell packs

Every LGTM reference-evaluator read path was reworked to *borrow* label/attribute strings (and histogram bucket buffers) from a backing store the evaluator holds, materializing owned values once at each public boundary. Motivation: the evaluators cloned every label value per row (N clones collapsing to M series); `by`/`without` re-cloned owned strings during aggregation. Full write-up: `.agents/docs/LABELSET_ARROW_REFACTOR.md`.

- **`LabelSet<'a> = Box<[(Cow<'a,str>, Cow<'a,str>)]>`** — borrowed values `Cow::Borrowed`, decoded ones (JSON escapes, hex ids) `Cow::Owned`; `into_owned` lifts at the boundary; content-based `Ord`/`Eq` keeps `BTreeMap` grouping semantics. An earlier `Arc<[(Arc<str>,Arc<str>)]>` cut was abandoned — it refcounts liveness the backing already guarantees (nested-`Arc` smell); `Cow` fits once the borrow is confined to the evaluator rather than living in a struct that outlives the batch.
- **`self_cell` packs** (`PromSeriesPack`/`PromHistogramPack`/`LogEntryPack`/`TracePack`) — the async `fetch` creates its backing internally, returning a self-owning pack with **no lifetime param** (owner `Box<dyn Any + Send>` type-erased so the pure model stays free of the facade DTO it borrows; dependent = borrowed rows). Crosses `async` cleanly; borrowing is synchronous-internal to the evaluator; the public `execute_*` returns stay owned + serde-safe.
- **Rule per field:** borrow heap/string fields; keep `Copy` scalars owned; the grouping `Vec`/`Box` stays owned (grouping rebuilds it).

Threaded through all three: PromQL (`PromSeries<'a>`, `HistogramPoint<'a>` borrows `ListArray` bucket buffers, whole reference evaluator generic over `'a`; `FloatSample` stays owned — `Copy`, re-sorted). LogQL (`LogEntry<'a>` borrows stream labels *and* `line`; `log_labels` owns the schema label name, borrows the attribute value). TraceQL (`SemanticValue<'a>`/`TypedAttributes<'a>`/`SemanticSpan<'a>`/`SemanticTrace<'a>`; AST fixes `SemanticValue<'static>` so the parsed query stays out of the lifetime; `compare_value` takes a single `'a`, covariance coerces the literals; events/links JSON-parsed → `into_owned`). Deferred-hex ids: span/trace ids are raw `Copy` `SpanId`/`TraceId` keys through the evaluator (simplified `structural`/`has_ancestor`), hex only for selected spans at the output; added `Ord`/`PartialOrd` to `SpanId`/`TraceId`. New dep: `self_cell` 1.3 (macro-only, zero transitive deps), contained to `imbh-lgtm` plus a two-line `imbh_core::ids` derive.

Context that frames the work: storage at rest is already Arrow (the write buffer holds a `RecordBatch` per ingest, IOx-style; segments are Parquet → Arrow). The remaining materialization is at the read *boundaries* (facade DTOs), which is what Level-2 attacks.

### Level-2 (raw-Arrow) LogQL read: benchmarked, root-caused, wired

Level 2 borrows raw `RecordBatch` buffers, skipping DTO materialization + the JSON attribute parse. Artifacts: `LogsApi::query_batches` (facade — the log scan minus `LogEntry` materialization; uses `SELECT *` so promoted attribute columns are present), `imbh_lgtm::StreamLabelReader` (resolves the stream schema to Arrow columns once per batch; reads promoted/service values borrowed zero-copy, parses the JSON blob only for non-promoted keys), and `crates/imbh-lgtm/examples/logql_level2.rs` (a counting-allocator A/B benchmark).

Initial result (5000 log rows, 4-label schema, current_thread):

- Fully-promoted schema: L2 = 202,800 allocs / 24.3ms vs DTO 338,475 / 29.2ms — 1.67x fewer allocations (~136k saved), ~17% faster. No per-row JSON parse; label values borrow the promoted dictionary buffers.
- Non-promoted schema (initial, per-key fallback): L2 = 354,648 allocs vs DTO 277,937 — 0.78x, a regression.

Root cause of the regression (decomposed benchmark, fetch vs label-extraction measured separately):

    fully promoted:  fetch  query_batches 167,797 | query(+materialize) 283,474
                     labels DTO 55,001 | L2 per-key 35,003 | L2 parse-once 35,003
    not promoted:    fetch  query_batches 106,622 | query(+materialize) 222,937
                     labels DTO 55,001 | L2 per-key 248,031 | L2 parse-once 121,031

The naive fallback called `json_get` **once per label key**, re-parsing the *entire* `attributes` blob N times per row (N=3 non-promoted attrs) — 248k label allocs, ~4.5x the DTO. **Not** `SELECT *`: `query_batches` (106k) is far cheaper than `query` (222k) non-promoted, so extra columns were never the cause. Fixing the reader to parse each blob **once per row** and extract all keys (now the default `StreamLabelReader::labels`; `labels_per_key` kept as diagnostic) halved label allocs (248k → 121k).

End-to-end with parse-once: fully promoted L2 202,800 vs DTO 338,475 → 1.67x fewer; not promoted L2 227,653 vs DTO 277,938 → 1.22x fewer (was a regression with per-key). So the regression was an artifact of the naive per-key fallback, **not** inherent to Level 2 — `query_batches` skips `LogEntry` materialization and the reader parses only the one blob its labels need.

Wired: `ImbhLogSource::fetch` uses the raw-Arrow path (`LogsApi::query_batches` + parse-once `StreamLabelReader`), with `LogEntryPack` owning `Vec<RecordBatch>` (was `Vec<imbh::LogEntry>`). `time`/`body`/labels are read straight from batch buffers; `LogEntry` materialization and the old DTO-based `log_labels` are gone from the LogQL path. `StreamLabelReader` was reworked to own its label names (so its lifetime ties only to the batch, not the shorter-lived schema — required for the `self_cell` `for<'inner>` builder). Behavior unchanged: `execute_logql` reference tests pass identically; the example's per-row assert confirms DTO == L2 label content.

### Metric Level-2 (raw-Arrow) read: benchmarked, wired

The PromQL analog. Added `MetricsApi::points_batches` (facade — the point scan minus `MetricPoint` materialization), `imbh_lgtm::metric_labels_from_batch` (parses the `attributes` blob once per row, lifting string entries; `service`/`__name__` read borrowed from columns), and `imbh_test_support::otlp_gauge_attrs` (multi-attribute gauge fixture). Benchmark: `examples/metric_level2.rs` (decomposed, counting allocator).

Result (5000 gauge points, 5 attrs): the label step is a **wash** (DTO 90,001 vs L2 91,001 allocs) — as predicted, because PromQL's *open* label set forces the blob parse regardless, so promoted dictionary columns don't help the source (unlike LogQL's fixed schema). **The whole win is skipping `MetricPoint` materialization**: fetch `points_batches` 80,537 vs `points` 166,549 (the DTO builds a typed attribute map + several `String` clones per point). End-to-end 1.50x fewer allocations non-promoted, 1.37x fully-promoted.

Wired: `PromSeriesSource::fetch`/`fetch_histograms` use `points_batches`; `scalar_series`/`histogram_series` are batch readers (column indices: `point_time`=0/Int64, `service`=2, `attributes`=3, `temporality`=4, `is_monotonic`=5/Bool, `value`=6/Float64; histogram `explicit_bounds`=6/`bucket_counts`=7 as `ListArray`). `HistogramPoint<'a>` bucket lists are **borrowed slices** of the scan's `ListArray` values buffers (`list_f64_slice`/`list_u64_slice`, via child-values + offsets — true zero-copy). Pack owner is `Vec<RecordBatch>`; the DTO `metric_labels` is gone. PromQL reference tests (incl. `histogram_quantile`) pass identically. `points()` stays as the public facade API.

### TraceQL: streaming evaluation + sound predicate pushdown

TraceQL is per-trace independent (each trace evaluated in isolation). This is the language/execution view; trace-query candidate-selection *correctness* has its own doc — cross-reference it.

**Streaming.** `fetch_complete_traces` (which materialized *all* candidate traces into one pack) was replaced by a streaming `TraceSource`: `fetch_candidates` returns candidate ids, then `execute_traceql` pulls each trace via `fetch_trace` (a single-trace `TracePack`), evaluates it, and drops it before the next. Peak memory dropped from the whole candidate set to **one trace**; the pack is now single-trace. Output identical.

**Predicate pushdown.** `candidate_filters(&SpansetExpr) -> Vec<SpanCandidateFilter>` extracts a *necessary* single-span filter and threads it via `TraceFetchRequest.candidate` → `build_trace_query` → the fixed semi-join candidate query, so traces that cannot match are skipped in storage. Sound by construction and unit-tested: bare `Select` lifts its pushable conjunction (same span); `And`/`Structural`/`count>=1`/`countAtLeast(>=1)` push one necessary side; `Or`, `count<=`/`==0`/`countAtLeast(0)`, `Ne`/`Regex`/non-`Span`-scope/numeric/intrinsic leaves push **nothing** (→ evaluate all). Partial pushdown of a conjunction is kept (a subset of a necessary AND is still necessary). It rides the trace_start-correct semi-join, so pushing a predicate cannot drop a trace whose root is in range but whose matching span is later; all user values stay parameterized (`p.str`/`p.i64`/`p.attr_field`). Level 3 (multi-query set algebra) was deliberately not built: extra `search()` scans can cost more than streaming all candidates, and per-trace eval is cheap.

### Level-2 payoff, summarized

- **LogQL** (fixed stream schema → promoted labels zero-copy): 1.22–1.67x fewer allocations. Wired.
- **Metric** (open label set → blob parse unavoidable; win is skipping `MetricPoint` materialization): 1.37–1.50x. Wired.
- **Trace** (streams one small trace at a time): marginal — measured ceiling 28% of `get`, realizable ~10–15% since the JSON parse is kept. Measured and declined. Measure the ceiling before building the reader.

Design principles that hold: `Arc` was the wrong tool, `Cow` + a held backing is right (reference-counting duplicates liveness the `RecordBatch`/DTO already guarantees; the lifetime stays contained because public outputs materialize owned). `self_cell` crosses the async boundary because the pack has no lifetime param. Level-2 payoff scales with rows-per-query and with whether labels are dict-promoted.

## Files

- `crates/imbh-lgtm/src/lib.rs` — crate root, flat re-exports of the full public surface.
- `crates/imbh-lgtm/model/logql.rs` — LogQL model; `LogFilter::LineMatches`/`LineNotMatches`; reference evaluator calling `imbh_core::matches_terms`.
- `crates/imbh-lgtm/src/syntax/logql.rs` — LogQL parser; bare-selector top-level branch; `|?`/`!?` line-filter loop.
- `crates/imbh-lgtm/src/source.rs` — native-IMBH adapter (feature `source`); `apply_log_filter`, `ImbhLogSource`, `PromSeriesSource`, `TraceSource`, the `*SemanticsExt` traits, `build_log_query`.
- `crates/imbh-lgtm/src/batch.rs` — Arrow result surface (feature `source`); long-form schemas + builders; `view_builder()` / `with_deduplicate_strings()`; `*_schema()` accessors.
- `imbh-lgtm` model/eval sources for PromQL/LogQL/TraceQL — `LabelSet<'a>`, `PromSeries<'a>`, `HistogramPoint<'a>`, `LogEntry<'a>`, `SemanticValue<'a>`/`SemanticSpan<'a>`/`SemanticTrace<'a>`, `PromSeriesPack`/`PromHistogramPack`/`LogEntryPack`/`TracePack`.
- `crates/imbh-lgtm/examples/logql_level2.rs` — LogQL Level-2 counting-allocator A/B benchmark.
- `crates/imbh-lgtm/examples/metric_level2.rs` — metric Level-2 decomposed benchmark.
- `crates/imbh-lgtm/Cargo.toml` — three explicit `[[example]]` entries (`logql_level2`, `metric_level2`, `trace_level2_ceiling`), each `required-features = ["source"]`.
- `imbh/logs.rs` — native SQL rendering (`StringPredicate::Matches`/`NotMatches`); the ungated `LogsApi::query_batches -> Vec<RecordBatch>` that the Level-2 read path binds to (distinct from the `proto`-gated `query_batches_with_stats`).
- Facade metrics API — `MetricsApi::points`, `MetricsApi::points_batches`.
- `imbh-index` provider — `provider.rs:219` (body-conjunct combine), `matches_text_terms`, `search_body_bool(dir, must, must_not)`.
- `imbh_core` — `matches_terms`; `ids` (`SpanId`/`TraceId` with `Ord`/`PartialOrd`); `imbh-core/src/attributes.rs` (earmark for `AttributesView`).
- `imbh_test_support::otlp_gauge_attrs` — multi-attribute gauge fixture.
- `.agents/docs/ARROW_LGTM_API_PROPOSAL.md` — Arrow surface design + Phase 3 findings.
- `.agents/docs/LABELSET_ARROW_REFACTOR.md` — borrowed-read design write-up.

## Test Coverage

- **LogQL dialect:** parser tests (bare selector, mixed operators, braceless `|?`, metric form still parses); model tests (term-AND vs substring, `!?` negation); source e2e (LogQL `|?`/`|=` filter a real ingested list with term-not-substring); IMBH tests (`StringPredicate::Matches` index-accelerated + `used_index`, `NotMatches` complement); provider tests (pure-`!?` and `+must -must_not` RowSelection, asserting `+alpha -alpha → empty`). `full_text_matches_in_sql` proves `matches("err") == 0` (substring is not a term match).
- **TraceQL:** operator-precedence + paren-override tests; predicate-pushdown unit tests over `candidate_filters` (which leaves push vs push-nothing).
- **Arrow surface:** 3 `batch.rs` unit tests (long-form one-row-per-sample + label Map/Utf8View, trace_id + span_ids List, empty-carries-schema); `repeated_labels_share_one_buffer_copy_across_samples` (200 samples of a 24-byte value → one buffer copy).
- **Level-2:** LogQL/metric example benchmarks include per-row correctness asserts (DTO == L2 label content); `execute_logql` and PromQL (incl. `histogram_quantile`) reference tests pass identically before/after wiring.
- **Injection:** injection-shaped execution regressions cover metrics and logs; native compiler tests assert user strings never appear in emitted SQL.
- Crate test counts observed: 38 default / 41 with `source` at consolidation; 50–51 with `--features source` through the Arrow work. Full-gate green throughout: `cargo fmt --all --check`, `cargo build --workspace`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace` (workspace 48/48 then 49/49 test binaries through the borrowed-read + Level-2 + TraceQL work).

## Pitfalls

- **`|?` must render against the bare column.** The provider's index pushdown (`matches_text_terms`) only claims `matches(<bare column>, ?)`. Wrapping the field in `coalesce(f,'')` (as the substring predicates do) makes it index-ineligible. Term ops render against bare `field`.
- **Selectivity cost gate is `>=` at 0.5.** A term hitting exactly half the rows gates to a full scan (`None`), not a RowSelection. Choose test fixtures with strictly < 0.5 selectivity (e.g. 2/6).
- **The per-key JSON fallback re-parses the whole `attributes` blob once per key.** This regressed non-promoted LogQL to 0.78x. Always parse each blob once per row and extract all keys (`StreamLabelReader::labels`, not `labels_per_key`). Reasoning alone had mis-called this a regression; a decomposed counting-allocator benchmark was needed.
- **`StreamLabelReader` must own its label names**, not borrow the schema — its lifetime must tie only to the batch, for the `self_cell` `for<'inner>` builder.
- **`Arc<[(Arc<str>,Arc<str>)]>` for `LabelSet` was rejected** — nested `Arc` refcounts liveness the backing already guarantees. `Cow` + a held backing is the right tool once the borrow is confined to the evaluator.
- **After a dependency-graph edit, a `--workspace` failure may live in a crate you never touched** — feature unification can newly exercise a latent `cfg`-gated defect. Check the trigger before assuming your change is at fault.
- **Auto-discovered examples inherit no feature gate.** All three `examples/*.rs` use `imbh` facade re-exports and drive a `Db`, but the `imbh` dependency is `optional = true` (pulled in only by `source = ["dep:imbh"]`), so with no `[[example]]` entries in `Cargo.toml` a focused `cargo clippy/test -p imbh-lgtm --all-targets` failed to compile them. The workspace gate masked it via feature unification. Any example touching an optional dependency needs an explicit `[[example]]` entry with `required-features`; verify with a **focused, default-feature** `-p <crate> --all-targets` run, which is exactly the command the workspace gate does not give you.
- **Upstream boundary rules beat local intuition, and the tests may be wrong.** Both the PromQL left-open lookback and the TraceQL absent-attribute negation were test-enshrined divergences: the fix required editing tests that asserted the buggy behaviour. When aligning a reference evaluator, read the upstream implementation's drop rule (Prometheus `vectorSelectorSingle`, Tempo's condition matching) rather than reasoning from set logic.

### Decisions to revisit only with a driver

- **Arrow output-side scan-buffer aliasing (Phase 1b full):** unblocked by the `Cow`/`self_cell` refactor but marginal over the string-view dedup that already landed (only bare selectors alias; aggregations reconstruct labels). See `ARROW_LGTM_API_PROPOSAL.md`.
- **`AttributesView` (Level-2 for the public facade DTOs / TUI / server):** highest leverage, biggest and breaking; already earmarked at `imbh-core/src/attributes.rs`.
- **Arrow Phase 3 (eval-in-DataFusion):** revisit only with a concrete driver (working set too large for bounded in-memory eval) and reference-differential tests. LogQL count/rate and PromQL rate-as-UDAF are the feasible cases; TraceQL structural matching stays in Rust.
- **TraceQL pushdown Level 3 (multi-query set algebra):** extra `search()` scans can cost more than streaming all candidates; per-trace eval is cheap.
- **Trace Level-2 (raw-Arrow trace read):** measured ceiling only 28% of `get`, realizable ~10–15%; declined.
- **Direct proto → Arrow ingest (skip transient `*Row` structs):** a footprint-vs-modularity trade.
