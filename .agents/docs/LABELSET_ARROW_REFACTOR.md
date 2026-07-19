# LGTM borrowed-read refactor (as-built)

Status: landed (2026-07-22), gated green. This supersedes the original `Arc`-based plan in this file
(kept in git history) — that approach was abandoned mid-flight in favor of lifetime-bound borrows.

## Goal

Stop the LGTM reference evaluators from cloning label/attribute strings (and histogram buffers) per
row. Read them borrowed from a backing store the evaluator holds, and materialize owned values
**once**, at each public boundary.

## Why not `Arc` (the abandoned first cut)

The first attempt made `LabelSet` `Arc<[(Arc<str>, Arc<str>)]>`. That reference-counts liveness the
backing store *already* guarantees: Arrow buffers (and the materialized DTOs built from them) live
behind the pack we hold for the evaluation. So a plain borrow is sound and cheaper than an `Arc`. The
smell that killed it: nested `Arc` mixes two sharing strategies (whole-set sharing + per-string
sharing) with no interner to justify the inner one.

## Core mechanism

- **`LabelSet<'a>` = `Box<[(Cow<'a, str>, Cow<'a, str>)]>`.** Borrowed values are `Cow::Borrowed`
  (straight from the backing); values that must be decoded (JSON escapes, hex ids) are `Cow::Owned`.
  `Box<[_]>` not `Vec` — the set is immutable, so drop the capacity word. `into_owned(self) ->
  LabelSet<'static>` lifts at the boundary. Derived `Ord`/`Eq`/`Hash` compare **by content**, so a
  borrowed and an owned set with equal strings are equal (`BTreeMap` grouping keys keep semantics).
- **`self_cell` packs** (one small crate, zero transitive deps). The source's async `fetch` *creates*
  its backing internally, so it can't return references to it — instead it returns a self-owning pack
  with **no lifetime parameter** (owner = `Box<dyn Any + Send>` type-erased backing; dependent =
  borrowed rows). The pack crosses `async` cleanly; all borrowing is synchronous and internal to the
  evaluator, which holds the pack and reads `borrow_dependent()`.
- **The lifetime never goes viral.** The public `execute_*` return types stay owned (`<'static>`) and
  serde-safe; `'a` lives only inside the evaluator, from `fetch` to the single `into_owned` at return.
- **The rule for each field:** borrow heap-buffer / string fields; keep `Copy` scalars owned; the
  outer grouping `Vec`/`Box` stays owned because grouping rebuilds it.

## What each path does

- **PromQL** — `PromSeriesPack` / `PromHistogramPack` own `Vec<MetricPoint>`. `LabelSet<'a>` borrows
  the point's attribute strings; `HistogramPoint<'a>` borrows the `ListArray` bucket buffers
  (`&'a [f64]` / `&'a [u64]`, `Copy`). `'a` threads through the whole reference evaluator
  (`execute_prom`, `eval_prom_at`, `select_instant`, `aggregate_instant`, `bounded_series`,
  `eval_histogram_quantiles`) — `by`/`without` on a borrowed set are pointer copies, not string
  copies. `FloatSample` stays owned (`Copy` scalar, re-sorted per series — nothing to borrow).
- **LogQL** — `LogEntryPack` owns `Vec<imbh::LogEntry>`. `LogEntry<'a>` borrows both its stream
  labels and its `line` (`&'a str`); `log_labels` owns the low-cardinality schema label *name*
  (`Cow::Owned`) and borrows the high-cardinality attribute *value* — one `'a` covers both.
  `LogPipelineState.current_line` stays `String` (the pipeline mutates it).
- **TraceQL** — `TracePack` owns `Vec<imbh::Trace>`. `SemanticSpan<'a>` borrows its strings;
  `TypedAttributes<'a>` / `SemanticValue<'a>` (`Cow` str/bytes) borrow attribute keys/values. The AST
  fixes `SemanticValue<'static>` (query literals are owned) so the parsed query stays out of the
  lifetime; `compare_value`/`semantic_partial_cmp` take a single `'a` and covariance coerces the
  `'static` literals down so `actual == expected` type-checks. Events/links are JSON-parsed, hence
  inherently owned (`SemanticValue::into_owned`). **Deferred-hex ids:** `span_id`/`trace_id` are raw
  `Copy` `SpanId`/`TraceId` map keys through the evaluator (which *simplified* `structural`/
  `has_ancestor` — no `.as_str()`/`.clone()`); hex is encoded only for *selected* spans at the
  `TraceQueryMatch` boundary. (`SpanId`/`TraceId` gained `Ord`/`PartialOrd`, needed as set keys.)

## What stays owned (correctly)

Public `execute_*` outputs (`Vec<PromSeries<'static>>`, `Vec<LogSeries<'static>>`,
`Vec<TraceQueryMatch>`), the parsed query AST (`SpanPredicate` literals), and the `*_batches` Arrow
output path (materializes into `StringViewBuilder`).

## Remaining follow-ups

- **Level 2 (raw-Arrow)** — packs currently own the *materialized DTOs*, not raw `RecordBatch`es. One
  Arrow→DTO copy already happened at `points()`/`query()`; borrowing straight from the batch buffers
  would eliminate it *and* skip the JSON attribute parse for promoted keys. The highest-leverage
  lever (helps every path at once), but the largest: needs the facade to expose its batches to
  `imbh-lgtm`, and a borrowing accessor over the promoted dictionary columns + the attributes blob.

Non-goal: a vectorized/columnar evaluator (the reference kernels are deliberately row-oriented).
