# Full-Text Search: Tantivy Index and the Cost-Gated Parquet RowSelection Bridge

## Summary

Full-text `matches(col, query)` search is powered by a per-segment Tantivy index (`<segment>.tidx`) built at seal, a shared pure-Rust tokenizer in `imbh-core`, and a cost-gated `Tantivy → Parquet RowSelection` bridge in a custom DataFusion `TableProvider`. `imbh-index` is the only crate that knows Tantivy; `imbh-query` is the only one that knows DataFusion. The index is always a best-effort accelerator: `matches` is pushed `Inexact` so DataFusion re-applies a shared-tokenizer UDF above the scan, making pruned and full-scan results provably identical.

## Key Facts

- The shared tokenizer lives in `imbh-core::text` (`tokenize`, `matches_terms`), not in `imbh-index` — it is a pure text primitive shared by the `imbh-index` analyzer and the `imbh-query` fallback, avoiding a query→index sibling edge.
- `matches` semantics: tokenized-term containment with implicit AND. `matches('timeout error')` requires both terms present, order-free. Substring is not a match (`'err'` != `error`). Lowercase + split on non-alphanumeric, no stemming/stopwords.
- `imbh-index` is the only crate that knows Tantivy; `imbh-query` is the only crate that knows DataFusion.
- `imbh-storage` depends on `imbh-index` so `seal` can build the sidecar — a deliberate intra-layer edge (storage orchestrates; index provides the primitive; storage never learns Tantivy).
- The `.tidx` sidecar is write-once: built in a single commit per sealed/compacted segment, rebuilt wholesale on compaction.
- The index writer uses `NoMergePolicy` (no background merge threads), consistent with imbh's no-background-threads guarantee.
- Crate count: 275 with `search` on (at the ≤ 275 target ceiling, hard limit 300), 216 with `search` off (Tantivy dropped).
- Pushing `matches` as `Inexact` (not `Exact`) is the safe choice for a union provider whose buffer half has no index.

## Details

### Shared tokenizer and the differential guarantee

`imbh-core::text` provides `tokenize` and `matches_terms` — pure, no Tantivy: lowercase + split on non-alphanumeric, no stemming/stopwords. `imbh-index` wraps `imbh_core::tokenize` as a Tantivy analyzer (`ImbhTokenizer`). `imbh-query` registers a `matches(Utf8, Utf8) -> Boolean` scalar UDF (the row-wise fallback) that calls `imbh_core::matches_terms`; it casts the text column to `Utf8` defensively because segment reads can arrive as `Utf8View`.

Because both the index path and the UDF fallback share `imbh_core::tokenize`, `matches` results cannot depend on flush timing (buffer vs sealed). The differential guarantee is tested by `index_matches_rowwise_fallback`, which asserts `search_body(index, terms)` equals the row set from `matches_terms` over the same bodies, across a query matrix.

### Per-segment index build

`imbh-index` builds a per-segment index: `body` tokenized WithFreqs, `service`/`severity_text` raw, `row` u64 fast field, nothing stored. `search_body` returns sorted row ordinals via the `row` fast field. Reading the row ordinal back is a fast-field lookup per hit (`segment_reader.fast_fields().u64("row").first(doc_id)`), grouped by `DocAddress.segment_ord` — realizing the "row ordinal as data, not doc-order assumption" model.

Tantivy 0.26's custom `Tokenizer` uses a GAT (`type TokenStream<'a>`); a `Vec<Token>`-backed stream implementing `advance`/`token`/`token_mut` is the simplest adapter over an external tokenizer.

Seal builds the sidecar: `imbh-storage::seal` writes `<segment>.tidx` alongside the `.parquet` via `imbh_index::build_logs_index`, with row ordinal = position in the time-sorted rows = Parquet row order.

The build path was later refactored so `build_logs_index` and `build_spans_index` both delegate to a shared `build_text_index(index_dir, docs)` over an `IndexDoc { body, service, severity_text, row }` stream. Spans index the tokenized span `name` into the same `body` field, so `search_body` serves both unchanged. The logs index format is byte-identical after this refactor (no regression).

### Cost-gated Tantivy → Parquet RowSelection bridge (`imbh-query/src/provider.rs`)

`LogsProvider: TableProvider` unions the buffer snapshot with the sealed segments. `supports_filters_pushdown` claims **`Inexact`** for `matches(<col>, '<lit>')` conjuncts — DataFusion keeps a `FilterExec` with the `matches` UDF above the scan, so the index is a pure accelerator and Parquet stays ground truth.

In `scan`, body terms from the pushed `matches(body, …)` conjuncts drive `imbh_index::search_body` per segment. **Cost gate:** if the hit fraction is `< 0.5`, a Parquet `RowSelection` (built by coalescing sorted hit ordinals into skip/select runs) reads only the matching rows; otherwise the whole file is read. Index-less segments and non-`body` matchers fall through to a full scan.

Segments are read via the `parquet` crate's `ParquetRecordBatchReaderBuilder` (`with_row_selection`) rather than DataFusion's `read_parquet` — this both applies the selection and yields `Utf8` (not `Utf8View`) columns, so `coerce` is an identity in the common path.

The facade pairs each `SegmentRef` with its `<segment>.tidx` sidecar (when `is_dir()`) into a `SegmentInput` and hands them to `run_sql`.

**Correctness argument:** pruning only ever happens for top-level AND `matches` conjuncts (an OR containing `matches` is one unrecognized `Expr` → `Unsupported` → not pushed), and DataFusion re-applies every `Inexact` filter via the shared-tokenizer UDF. So a wrong/over-eager selection can only remove rows that would have failed the re-check anyway — never change results. Claiming `Inexact` (not `Exact`) is essential for a union provider whose buffer half has no index: `Exact` would drop the filter from the plan and leave buffer rows unfiltered.

### Per-segment span search

Spans originally carried `text_column: None` (span search on name/attrs was a follow-up), so `traces().matches(name)` worked only as a scan. Completion spanned 3 crates:

- **imbh-index** — `build_spans_index` delegates to the shared `build_text_index`, indexing the tokenized span `name` into the `body` field.
- **imbh-storage** — a `build_spans_sidecar` helper pair mirrors `build_logs_sidecar` (gated behind `search`, no-op without). `write_spans_segment` builds the span `.tidx` at seal; the compaction path dispatches on table (`Table::Spans` → span sidecar, else logs) and span compaction flipped from `build_index: false` to `true`. `spans_batch_to_index_rows` extracts name/service from a compacted spans batch (mirrors `logs_batch_to_index_rows`).
- **imbh (facade)** — spans `text_column: None` → `Some("name")`, so `matches(name, …)` drives the RowSelection bridge over the span sidecar. The facade derives `index_path` from `.tidx` existence, so no other change; under `search`-off no sidecar is written → plain scan.

Minor accepted cost (matches the logs pattern): a `search`-off compact still computes `spans_batch_to_index_rows` before the no-op sidecar discards it.

### Index merge policy and shutdown join

Both fixes were motivated by upcoming scheduled `seal()` calls, which run index builds on the background maintenance thread mid-flight.

**1. `NoMergePolicy` on the index writer (imbh-index).** The `.tidx` is write-once, so Tantivy's default `LogMergePolicy` is a poor fit: it spawns its own background merge threads per writer (contradicting imbh's no-background-threads guarantee), and `IndexWriter::Drop` then kills any in-flight merge (`segment_updater.kill()`), abandoning partial-merge segment files that nothing GCs (imbh runs no Tantivy GC). `NoMergePolicy` spawns no merge thread at all: `Drop` is trivially clean, no seal blocks on a merge, and search reads across whatever sub-segments the commit flushed (bounded by the seal threshold; still an accelerator with a UDF re-check). The interim `wait_merging_threads()` was reverted — it would have blocked every scheduled seal on its merge, the exact mid-flight stall to avoid. If single-segment indexes are wanted for very large segments, do one synchronous `writer.merge(&ids)` — deterministic, no background thread.

**2. `Db::close()` joins the background maintenance thread (facade).** The opt-in maintenance thread was `thread::spawn`ed with its `JoinHandle` discarded — it self-terminated via a `Weak`/`closed` flag, so `close()` could only set the flag and return while a scheduled seal was still running. Now the handle is kept (`Inner.maintenance_handle: Mutex<Option<JoinHandle>>`), and `close()` sets `closed`, takes the handle out of the lock, and `join()`s it before the final seal — a genuine synchronous shutdown that waits for any in-flight scheduled seal (index build included) to finish. It is idempotent (the `closed` swap guards double-join). The blocking facade's `close()` delegates and inherits this.

Architectural note: the index writer is per-build (created→committed→dropped inside `build_text_index`), so there is no long-lived writer for a shutdown hook to await — the shutdown concern lives at the `Db`/maintenance-thread level, and the per-build merge concern lives at the merge-policy level.

## Files

- `imbh-core` (`imbh-core::text`) — shared `tokenize` and `matches_terms` primitives.
- `imbh-index` — the only Tantivy-aware crate: `ImbhTokenizer` analyzer, `build_text_index`, `build_logs_index`, `build_spans_index`, `search_body`, `NoMergePolicy` writer config, `IndexDoc { body, service, severity_text, row }`.
- `imbh-query` (`imbh-query/src/provider.rs`) — the only DataFusion-aware crate: `LogsProvider: TableProvider`, `matches(Utf8, Utf8) -> Boolean` scalar UDF, cost-gated `RowSelection` bridge, `SegmentInput`, `run_sql`.
- `imbh-storage` — `seal`, `write_spans_segment`, `build_logs_sidecar`, `build_spans_sidecar`, `logs_batch_to_index_rows`, `spans_batch_to_index_rows`, compaction dispatch on `Table::Spans`; depends on `imbh-index`.
- `imbh` (facade) — pairs `SegmentRef` with `.tidx` into `SegmentInput`, derives `index_path` from `.tidx` existence, sets span `text_column` to `Some("name")`, `Inner.maintenance_handle: Mutex<Option<JoinHandle>>`, `Db::close()`.

## Test Coverage

- `index_matches_rowwise_fallback` (imbh-index) — asserts `search_body(index, terms)` equals the `matches_terms` row set over the same bodies, across a query matrix (the differential guarantee).
- `matches_over_sealed_segment_and_buffer` (imbh-query) — exercises the pruned path (`'error'` = 1/4, below threshold), the full-scan path (`'request'` = 2/4, at threshold), and buffer ∪ segment.
- `traces_search_by_name_hits_sealed_index` — end-to-end on disk: seal → search hit (`"cart"` in `"GET /cart"`) + miss (`"zzznomatch"`) + compaction (two span segments merged, `.tidx` rebuilt, both traces still found). Runs clean on both feature configs (default and `imbh --no-default-features`).
- `background_maintenance_auto_seals` — extended: after auto-seal, `close()` returns (no hang on the join), is idempotent, and ops are rejected afterward.
- The imbh-index build refactor is covered by its existing 2 tests (logs index format byte-identical, no regression).

## Pitfalls

- Do not claim `Exact` pushdown for `matches` on the union provider: `Exact` drops the filter from the plan and leaves the un-indexed buffer half unfiltered. `Inexact` is required so the UDF re-check runs.
- Do not put the tokenizer in `imbh-index`: it must stay in `imbh-core` so both the analyzer and the UDF fallback share it; otherwise a query→index sibling edge appears and flush-timing could change results.
- Do not use Tantivy's default `LogMergePolicy`: it spawns background merge threads (violating the no-background-threads guarantee) and `IndexWriter::Drop` kills in-flight merges, leaking un-GC'd partial-merge segment files. Use `NoMergePolicy`.
- Do not call `wait_merging_threads()` on the writer: it blocks every scheduled seal on its merge, the mid-flight stall to avoid. For single-segment indexes use a synchronous `writer.merge(&ids)` instead.
- Do not discard the maintenance thread `JoinHandle`: `close()` must `join()` it to guarantee a scheduled seal (index build included) has finished before shutdown returns.
- Segment reads may arrive as `Utf8View`; the `matches` UDF casts to `Utf8` defensively. Reading segments via the raw `parquet` crate (`ParquetRecordBatchReaderBuilder`) yields `Utf8` and sidesteps the `Utf8View` coercion that DataFusion's `read_parquet` forces.
- Substring is not a match: `'err'` does not match `error`. `matches` is tokenized-term containment, not substring search.
- Crate count sits exactly at the 275 target with `search` on (hard limit 300); weigh any new Tantivy-side dependency against it.
