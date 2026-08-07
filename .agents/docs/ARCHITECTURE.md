# IMBH Architecture

The canonical design reference for IMBH — the data model, storage engine, search, query
layer, and the full public API surface **as built**. For orientation (vision, goals, non-goals,
prior art, status), start with [OVERVIEW.md](./OVERVIEW.md).

> **Section numbering.** Sections here retain their numbers from the original design plan so the
> many `§N` cross-references in the code and docs stay valid: §5–§12, §14, §15, and the
> appendices live in this file; §1–§4 and §13 live in OVERVIEW.md.
>
> **Design vs implementation.** IMBH is built (M0–M6 complete). Where the code diverges from the
> original design intent, this document describes **what is actually built** and marks the gap
> with a *Deviation* or *Not yet implemented* note, so the doc is a faithful map of the code, not
> an aspiration. Verify implementation detail against `crates/**`; JOURNAL/LTM record why a
> deviation was made.

## 5. Architecture overview

```
                    ┌────────────────────────────────────────────────┐
 OTLP protobuf ───▶ │ ingest: decode → normalize → canonical rows    │
 (bytes or typed)   └───────────────┬────────────────────────────────┘
                                    ▼
                    WAL (framed raw OTLP, xxh3, fsync policy)
                                    ▼
                    mutable buffer (per-table Vec<Row>, capped)
                                    ▼  seal (explicit; auto-threshold dormant)
        ┌─────────────────────── segment (immutable) ────────────────────────┐
        │  <table>/<day>/<ulid>.parquet     (zstd, sorted by time, page index) │
        │  <table>/<day>/<ulid>.tidx/       (tantivy: body text + row-ordinal  │
        │                                    fast field; no attrs field)       │
        └──────────────────────────────┬──────────────────────────────────────┘
                                       ▼ registered in
              MANIFEST delta log + checkpoint (CURRENT → MANIFEST-<N>)
                                       ▼
       ┌───────────────────────────────────────────────────────────────┐
       │ query: SQL → DataFusion → custom TableProvider per table       │
       │   prune: manifest time ranges → parquet stats/page index       │
       │   matches() predicate → tantivy search → RowSelection (gated)  │
       │   residual filters/aggs → DataFusion (memory-pool bounded)     │
       └───────────────────────────────────────────────────────────────┘
```

Key properties:

- **Immutability everywhere after seal.** No tombstones, no in-place mutation. Crash safety
  reduces to WAL replay + manifest delta-log replay (torn tail dropped) + orphan cleanup on open.
- **Single writer, many readers — across processes.** A read-write open holds an exclusive advisory
  lock on `<dir>/writer.lock` (via `fs4`, an existing transitive dep), so a second writer — this
  process or another — fails fast with `Error::lock_held`; the lock releases on drop / process exit
  (no stale-lock cleanup). Read-only opens (`Db::open_read_only`, `Access::ReadOnly`) take no lock and
  never mutate the directory, so any number of reader **processes** coexist with the one writer. A
  reader cannot see the writer's in-RAM buffer, so it reconstructs a point-in-time snapshot per query:
  the manifest's segments unioned with the writer's live WAL tail (frames past the manifest
  watermark), materialized through the same replay path as open. Correctness rests on the **manifest
  re-check bracket** (`read_disk_snapshot`): read manifest → scan WAL → re-read manifest, retrying if
  the watermark advanced, which makes segments (`lsn ≤ W`) and replayed tail (`lsn > W`) provably
  disjoint (no double-count) and guarantees no frame `> W` was reclaimed yet (no drop). WAL visibility
  is via the shared OS page cache, so reads are near-real-time (fsync governs crash durability, not
  cross-process visibility). A writer advertises its WAL mode in a `db.info` marker at open; a
  read-only open against a **WAL-off** writer — which could only ever see seal-interval freshness — is
  rejected by default with `Error::reader_wal_disabled`, with `DbBuilder::allow_stale_reads()` to opt
  in. **Read-during-delete
  is handled (Phase 3):** if a query errors and a segment the snapshot named has since been unlinked
  by writer-side retention/compaction, the read-only path re-derives from the current manifest and
  retries (bounded by `READER_QUERY_TRIES`); a failure with every path still present is surfaced
  as-is. See §7.1 for the full cross-process design.
- **Queries see buffer + segments, atomically.** The writer's own query path takes one
  `Storage::query_snapshot` under a single lock, capturing every table's buffer *and* its segment set
  together, so a background seal cannot make a query double-count (a row read from the buffer and
  again from the just-written segment). Because seal writes the segment **off-lock** — leaving a
  window where rows are out of the buffer but not yet registered — the snapshot also unions the
  in-flight seal's staged batches (§7), so no query transiently drops them either. Data is queryable
  immediately on ingest, before any flush.
- **No background threads unless opted in.** Maintenance (seal, retention, compaction) runs
  inline via `db.maintain()`, or on one opt-in owned thread via
  `Maintenance::Background(interval)`. That thread observes a `closed` flag and is joined by
  `db.close()`, so shutdown is synchronous with any in-flight seal. This is a hard guarantee for
  embedders.
- **The flush scheduler picks *when* to seal (`FlushPolicy`, §10.2).** `Maintenance` says *who* runs
  the loop and how often retention passes; `FlushPolicy` says what makes it seal. Its triggers OR
  together — periodic (`every`), buffered bytes (`at_buffer_bytes`, defaulting to the
  memory-budget-derived threshold), buffered rows (`at_buffer_rows`), on-disk WAL size
  (`at_wal_bytes`), and idle (`after_idle`) — with `tick` setting how often they are evaluated and
  `FlushPolicy::manual()` disabling all of them. The same loop also honors `WalMode::Interval(d)` by
  fsyncing the WAL every `d`: without a scheduler running, interval mode can only fsync
  opportunistically on `flush`/`close`. A host that sets no policy keeps the historical behavior
  (seal on the maintenance interval + at the byte threshold). `imbhd` runs the scheduler by default
  (§10.16) because a collector that never seals holds every row in buffer + WAL.
- **Ingest is inline by default; async is opt-in.** By default ingest runs entirely on the caller's
  thread (decode → WAL append + fsync → buffer push), so data is queryable immediately and the
  receipt is durable under `WalMode::Always`. `Ingest::Async { handle, capacity, overflow }` (§10.5)
  offloads the WAL + buffer write to **one** worker task on a host-provided `tokio::runtime::Handle`
  (never an owned OS thread — an FFI host such as the Go binding drives it with its own threads). The
  decode still runs on the caller (so `accepted` and decode errors stay synchronous); only the write
  is deferred. The worker observes the same `closed` flag and is awaited by `db.close()` after the
  queue drains, so a clean shutdown loses no enqueued rows. The bounded queue's full behavior is
  chosen by `Overflow::{Block,Fail,DropOldest}`.

## 6. Data model

All tables share OTel's resource/scope split. Timestamps are `Timestamp(Nanosecond, UTC)`. All
text columns are stored as plain `Utf8`.

> *Deviation (dictionary encoding).* The original design specified `Dictionary(Utf8)` for
> low-cardinality columns (`service`, `severity_text`, `resource`, `scope`, `metric`, `unit`,
> span `name`/`kind`/`status_code`). The current schemas use plain `Utf8` throughout — no
> dictionary encoding anywhere. zstd reclaims most of the redundancy on disk; revisit dictionary
> encoding if in-memory buffer RSS or scan cost warrants it.

### 6.1 Attribute strategy (the load-bearing decision)

OTel attributes are open maps of `AnyValue`. Arrow/Parquet unions are a dead end and per-key
column explosion is an RSS/schema-churn trap. The design:

1. **Canonical JSON text column** per attribute scope: `attributes` (record-level), `resource`,
   `scope`. The **canonical form is a precise spec** (equal maps must serialize byte-identically):
   UTF-8; keys sorted by Unicode code point; no insignificant whitespace; integers as minimal
   decimal; doubles via shortest round-trip; `bytes` → base64; non-finite doubles → a reserved
   sentinel object `{"$f":"nan|inf|-inf"}`; nested `kvlist`/`array` recursively canonicalized.
   This is **one shared canonical encoder** in `imbh-core`, paired with a **dependency-free JSON
   parser** (the inverse), used by the segment writer, the metrics/attrs materializers, and the
   `json_get_str` UDF (§9.3). A property test asserts `canon(map) == canon(shuffle(map))`.
2. **Promoted `service` column.** `service.name` is always promoted to a `service` column on
   every signal (the pivot of all observability queries). The configurable `promote = [...]`
   typed-column feature is **now implemented** via `DbBuilder::promote(Promote::new([...]))`: each
   listed key becomes a nullable `Dictionary(Int32,Utf8)` column appended (uniformly, same order)
   to **every** signal schema, materialized at buffer-encode time from the row's canonical-JSON
   scopes with record `attributes` → `resource` → `scope` precedence (only string values promote;
   others → NULL). Keys colliding with a built-in column name are dropped. The promoted key **also
   stays in the JSON blob** — the column is a pushdown/zero-copy accelerator, not a relocation, so
   `json_get_str` / external `json_extract` / the reference label evaluators are unaffected. Adding
   or removing keys is backward-compatible on **both** the read and the compaction path: segments
   sealed before a key was promoted lack the column and are null-filled at query time by the `coerce`
   schema-evolution path (§9), **and are normalized to the current canonical schema again when
   compaction merges a day partition whose segments were sealed under different promote sets**. That
   second half is load-bearing and was once absent — `concat_batches` takes columns *positionally* and
   does not validate them against the schema it is handed, so merging mismatched segments used to
   panic (first segment wider), silently truncate (first segment narrower), or silently concatenate
   two differently-named promoted columns into one. Note the null-fill is not a back-fill: a promoted
   column is only projected from `attributes` at ingest, so pre-promotion rows stay NULL after
   compaction — which is exactly what a query over those same un-compacted segments already returned,
   so compaction never changes an answer.
   **Pushdown dispatch is wired**: the typed logs/metrics/traces query builders and the attribute
   discovery path route each record-`attributes` access through `SqlParams::attr_field`, which emits
   the promoted dictionary column (`CAST("key" AS VARCHAR)`, exactly like `service`) when the key is
   promoted and `json_get_str(attributes, $key)` otherwise — provably identical results, since the
   column mirrors the record `attributes` scope only and the key also stays in the JSON. Column
   values are string-only.
   Ahead of both branches, `attr_field` resolves the **built-in** `service` column: a key spelled
   `service` (the column name) or `service.name` (the OTel semantic convention) emits
   `CAST(service AS VARCHAR)`. Without that branch such a key falls through to
   `json_get_str(attributes, …)` and is NULL on every row — `service.name` lives in the resource,
   never in record `attributes` — so a *filter* on it matched nothing and a *group-by* collapsed
   into one empty-labelled series with every count merged, silently (a missing attribute is a
   legitimate NULL). The built-in branch wins over `promote`: a promoted key can never shadow
   `service` (reserved names are dropped by `promoted_columns`), and a promoted `service.name`
   column would be materialized from record `attributes` and hence be all-NULL for the same reason,
   so the built-in column is strictly the better answer for either spelling.

   *Not pursued — LGTM label-read zero-copy.* Sourcing PromQL/LogQL result-label buffers from the
   promoted columns (the original ARROW_LGTM_API_PROPOSAL Phase 3 idea) is architecturally bounded
   and was left out: PromQL treats **every** string attribute as a label, so the full `attributes`
   JSON is parsed regardless of what is promoted; the facade also materializes that parsed map into
   the public `MetricPoint`/`LogEntry` DTO fields unconditionally; and the reference evaluators build
   an owned `LabelSet(Vec<(String,String)>)`, so label strings are owned at that boundary no matter
   their source. Capturing real zero-copy would need either promoting the entire (unbounded)
   attribute set — the column-explosion anti-pattern this design rejects — or a breaking `LabelSet`
   redesign plus a parallel Arrow-native evaluator. The promotion payoff is therefore realized on the
   **filter/pushdown** side, not the label-read side.
3. **Attribute filtering** on non-promoted keys resolves via `json_get_str(attributes, 'k')`
   scans. *Deviation:* the original design indexed all attributes in a **Tantivy JSON field** for
   exact-term filtering (§8); that field is **not built**, so attribute predicates do not push
   into Tantivy — they run as UDF scans over surviving rows.
4. Access in SQL via `json_get_str(json, 'key')`. *Deviation:* only the string accessor exists;
   `json_get_int/float/bool` are not implemented (cast the string result in SQL if needed).

**RSS cost of the record-level `attributes` column.** Unlike `resource`/`scope`, per-record
`attributes` are effectively unique, so the mutable buffer holds full uncompressed
canonical-JSON UTF-8, one blob per row. This is why the buffer is bounded by **bytes, not row
count** (§7). Levers when it bites: lower the seal byte-threshold (flush sooner) or cap oversized
attribute maps. zstd reclaims most of it on disk at seal.

**Storage encoding — canonical JSON text (decided GO).** On-disk representation of
`attributes`/`resource`/`scope` and structured log bodies is canonical JSON text. Two binary
alternatives were evaluated and rejected: **CBOR** (marginal on-disk wins after zstd; costs the
zero-copy `segment_files` handoff to external tools that can `json_extract`, and has no
`cbor_get_*` UDF ecosystem) and **rkyv** (its zero-copy benefit is nullified inside a Parquet
cell with no alignment guarantee; non-portable, non-evolvable across a retention window and
external readers). Future path: the Parquet VARIANT logical type is the natural successor — a
storage-format change that keeps the UDF surface, not an API break.

### 6.2 Logs — table `logs`

| Column          | Type                          | Notes                              |
|-----------------|-------------------------------|------------------------------------|
| time            | Timestamp(ns, UTC)            | `time_unix_nano`; sort key         |
| observed_time   | Timestamp(ns, UTC), nullable  |                                    |
| service         | Utf8, nullable                | promoted `service.name`            |
| severity_number | UInt8                         | OTel enum                          |
| severity_text   | Utf8, nullable                |                                    |
| body            | Utf8                          | structured bodies → canonical JSON |
| attributes      | Utf8 (canonical JSON)         |                                    |
| resource        | Utf8 (canonical JSON)         |                                    |
| scope           | Utf8 (canonical JSON)         |                                    |
| trace_id        | FixedSizeBinary(16), nullable | correlation                        |
| span_id         | FixedSizeBinary(8), nullable  |                                    |
| flags           | UInt32                        |                                    |

### 6.3 Traces — table `spans`

| Column         | Type                     | Notes                                        |
|----------------|--------------------------|----------------------------------------------|
| trace_id       | FixedSizeBinary(16)      |                                              |
| span_id        | FixedSizeBinary(8)       |                                              |
| parent_span_id | FixedSizeBinary(8), null |                                              |
| name           | Utf8                     |                                              |
| kind           | Utf8                     | `SERVER`/`CLIENT`/…                          |
| start_time     | Timestamp(ns, UTC)       | sort key                                     |
| duration_ns    | UInt64                   | stored (not derived) → row-group min/max lets "slow spans" queries prune |
| status_code    | Utf8                     | `UNSET`/`OK`/`ERROR`                         |
| status_message | Utf8, null               |                                              |
| service / attributes / resource / scope | as in logs |                                 |
| events         | Utf8 (canonical JSON list), null | events/links kept inline               |
| links          | Utf8 (canonical JSON list), null |                                       |
| trace_state    | Utf8, null               |                                              |
| flags          | UInt32                   |                                              |

`end_time` is derived (`start_time + duration_ns`) at query time. *Deviation:* the original
design put **Parquet bloom filters** on `trace_id`/`span_id`; bloom filters are **not built** —
`traces().get(id)` relies on manifest time-range and row-group stats pruning plus a filtered
scan.

### 6.4 Metrics — one table per point kind

OTel's metric model is preserved, not squashed: `metrics_gauge`, `metrics_sum`,
`metrics_histogram`, `metrics_exp_histogram`, `metrics_summary`.

Shared columns: `time`, `start_time` (null where N/A), `metric` (Utf8), `unit` (Utf8),
`service`, `attributes`, `resource`, `scope`, `flags`, `temporality` (Utf8, null), and an
`exemplars` (Utf8 canonical JSON) column — present on every metric table (**not** feature-gated).
Kind-specific: `value: Float64` + `is_monotonic: Bool` (gauge/sum); histogram: `count: UInt64`,
`sum/min/max: Float64?`, `explicit_bounds: List<Float64>`, `bucket_counts: List<UInt64>`;
exp-histogram: `scale: Int32`, `zero_count`, `zero_threshold`, positive/negative `offset: Int32`
+ `counts: List<UInt64>`; summary: `count`, `sum`, and parallel `quantiles: List<Float64>` +
`values: List<Float64>`.

Metrics tables are **not** Tantivy-indexed (attr cardinality is low; dictionary/promoted columns
+ Parquet stats carry filtering).

**Temporality normalization — asymmetric, and only one direction is built:**

- *Cumulative counter → rate* is windowed and stateless: diff consecutive points within the
  requested range, clamp negative diffs as counter resets. Exposed at query time via
  `MetricQuery::…rate()` (per-second over delta) and `.rate_counter()` (over cumulative).
- *Delta → cumulative* is **stateful** (the running total depends on every prior point). The
  original design mandated normalizing delta sums/histograms to cumulative **at ingest** via a
  running accumulator keyed by series identity — the one place the immutable design was meant to
  bend. *Not yet implemented:* delta points are stored as they arrive; a delta series queried as
  a cumulative one is a **known gap** (tracked in [TODO.md](./TODO.md)).

## 7. Storage engine

- **WAL**: append-only files of framed records `(len, xxh3, monotonic_lsn, payload)` where the
  payload is the **raw OTLP export request bytes** plus a signal tag, and `xxh3` is an XXH3-64
  checksum over the frame (pure-Rust `xxhash-rust`; no C). fsync policy:
  `off | interval(1s, default) | always`. WAL can be disabled for cache-like deployments.
  **Idempotent recovery via a watermark:** the manifest records the highest LSN fully captured in
  sealed segments; replay on open **re-ingests only records with LSN > watermark**, so a crash
  after a seal but before WAL truncation does not double-count. Truncation of covered WAL is then
  a space-reclaim, not a correctness step.
- **Mutable buffer**: per-table `Vec<Row>`; ingest appends under a short mutex. A query snapshot
  (`Storage::query_snapshot`) is a consistent read of **all** tables' buffers *and* segment sets under
  one hold of that lock — buffer∪segments is atomic w.r.t. seal, so an interleaved seal cannot make a
  query double-count. It also unions each buffer with any in-flight seal's staged batches
  (`Inner::sealing`, keyed by a per-seal generation), which keeps rows visible during the off-lock
  segment write below — otherwise a snapshot taken mid-seal would find them in neither the buffer nor
  a segment (a transient *drop*). **Seal** does a `mem::take` of the row vectors and **builds the
  Arrow `RecordBatch` at seal time**. *Deviation:*
  the original design was an O(1) freeze-and-swap of live Arrow builders; the current design keeps
  rows as `Vec<Row>` and materializes Arrow at seal, so the seal cost is the batch build rather
  than a pointer swap. The buffer is bounded by **bytes** (per-row JSON dominates). Seals are
  triggered explicitly (`flush()`, `maintain()`, `compact()`, `close()`) or by the opt-in flush
  scheduler, whose default size trigger is that byte threshold (`seal_threshold_bytes`); a
  `FlushPolicy` can replace it with an explicit byte/row/WAL-size/idle trigger or a periodic one
  (§5/§10.2).
- **Seal path**: sort by time → write Parquet to a temp path (zstd level 3 default; page index
  on; **no bloom filters**) → build the Tantivy index in a temp dir → fsync → update the manifest
  → rename temp → final. A crash mid-seal leaves an orphan temp dir invisible to the manifest,
  cleaned on next open. The Parquet/index write runs **off the buffer lock** (so ingest is not
  blocked during encoding); the taken batches are held in `Inner::sealing` for that window so
  concurrent queries still see them, and the entry is dropped under the same lock that registers the
  segment — a query therefore sees each row exactly once (buffer, then staging, then segment).
- **Manifest**: an **append-only delta log with a compacted checkpoint** (`imbh-storage`'s `manifest`
  module). It is the sole source of truth for what is queryable; directory scans are never trusted.
  A tiny `CURRENT` file (atomically replaced write-temp → fsync → rename → fsync-dir) names the active
  log `MANIFEST-<NNNNNN>`; that log is a sequence of **framed** records (`len | xxh3 | payload`) whose
  first frame is a full-state **checkpoint** and whose later frames are small **deltas** (segments
  added/removed since, + an optional new watermark). A seal/retain/compact appends only its diff — so
  a persist is O(change), not O(total segments), retiring the old whole-file rewrite's write
  amplification. Once the log grows past a threshold it is **rolled**: a fresh `MANIFEST-<N+1>` is
  written starting with a checkpoint of the full current state, `CURRENT` flips to it, and the old log
  is unlinked — bounding both file size and reopen-replay cost. Crash safety mirrors the WAL: frame
  scanning stops at the first torn/checksum-failing frame (a half-appended edit is simply dropped, and
  its rows replay from the still-unreclaimed WAL), and a roll is atomic to readers via `CURRENT` (a
  reader resolves it, replays that one log, and re-resolves if a roll unlinked it mid-read — the new
  checkpoint holds everything). Durability ordering is unchanged: an edit is fsync'd before the WAL it
  supersedes is reclaimed or the segment files it drops are deleted. A legacy M0 whole-file `MANIFEST`
  is migrated to the delta-log format on the first writer open.
- **Partitioning**: `<table>/<UTC day>/` directories. Retention drops whole days plus manifest
  entries out of budget (oldest first). Out-of-order/late data lands in current segments; manifest
  time ranges may overlap and pruning uses ranges, not directory names.
- **Compaction**: merges small Parquet files within a partition and **rebuilds** the Tantivy
  index from the merged Parquet. Rebuild (vs. Tantivy segment merge) costs re-tokenization but
  makes row alignment correct by construction. Optional — a DB that never compacts is still
  correct. Built.
- **Retention**: age + max-disk-bytes, oldest-first. Built.
- **Directory fsync (platform note)**: every step above that makes a file *appear* (a new WAL segment,
  a `rename` of a Parquet temp into its day partition, the `CURRENT` swap) fsyncs the containing
  directory afterwards so the directory entry is durable, not just the file contents. That step is
  **skipped on Windows**, which offers no equivalent primitive: opening a directory as a file fails
  with `ERROR_ACCESS_DENIED` without `FILE_FLAG_BACKUP_SEMANTICS`, `FlushFileBuffers` takes a file
  handle, and the only volume-wide flush (`\\.\C:`) needs administrator rights and flushes the entire
  volume cache — not something a library can call. SQLite, LMDB and RocksDB all no-op their directory
  sync on Windows for the same reason (RocksDB's `WinDirectory::Fsync` is a literal `return OK`).
  Only the directory-entry ordering step is dropped; every file-content sync is unchanged (the WAL
  fsync policy above, the pre-rename segment and `CURRENT` syncs), so a `WalMode::Always` receipt on
  Windows still means the frame bytes were flushed. **What imbh does not independently guarantee on
  Windows** is that the *directory entry* for a just-created segment or a just-completed rename
  survives a power loss: that rests on NTFS's metadata journal being flushed in write-ahead order
  with the file's own flush, which is the prevailing understanding of NTFS but is not something this
  project has measured. The exposure is hard power loss only (a process crash is unaffected — the OS
  page cache still holds the entry), and the renames in the seal path and the `CURRENT` swap are the
  sensitive cases, not the WAL segment create.
- **Locking**: Built. A read-write open acquires an exclusive advisory lock on `<dir>/writer.lock`
  (`fs4::fs_std::FileExt::try_lock_exclusive`, non-blocking); a second writer fails fast with
  `Error::lock_held`. The lock releases on handle drop / process exit, so a crashed writer leaves no
  stale lock. Read-only opens (§5) skip the lock entirely. A read-write open also writes a `db.info`
  marker advertising its WAL mode, so a reader can detect a WAL-off writer and reject a read-only open
  that would get only seal-interval freshness (§5). See §7.1.

### 7.1 Cross-process concurrency (single-writer, many-reader)

One **writer process** owns a DB directory while any number of **separate reader processes** query it
and observe ingested data in **near-real-time** (within ~ms, not just at the seal interval). Out of
scope (non-goals, OVERVIEW.md §3): multiple concurrent writer processes, MVCC / serializable
isolation, cross-process point deletes or updates.

**Why this needs no new IPC.** A reader in another process cannot see the writer's in-RAM mutable
buffer, but it does not need to: everything a query requires already lands on disk as two artifacts
the single writer maintains, both already engineered to be read safely by an outside observer:

1. **Immutable Parquet segments**, listed by the **manifest** — an append-only delta log named by an
   atomically-replaced `CURRENT` pointer (§7). A reader resolves `CURRENT` (a whole-file rename, never
   torn) and replays that log's framed records, stopping at the first torn/checksum-failing frame, so
   it always reconstructs a consistent point-in-time segment set. Segments never mutate once written
   (§10.11).
2. **The WAL** — append-only framed records with an XXH3-64 checksum and a monotonic LSN (§7). Frame
   scanning already stops at the first torn / checksum-failing / non-monotonic frame and returns
   everything before it — exactly what a reader tailing a file the writer is actively appending to
   needs.

So a reader's queryable state is **manifest segments ∪ WAL-tail replay**, both on-disk,
self-describing, and crash/torn-tolerant. Cross-process visibility of just-appended WAL bytes comes
from the **shared OS page cache**: the writer's `write_all` is visible to a reader's `read` on the
same file immediately, before any fsync — fsync governs crash durability, not inter-process
visibility. Consequence: near-real-time reads require the WAL enabled; a `WalMode::Off` writer offers
readers only segment-level (seal-interval) freshness, which a read-only open rejects by default (see
below).

**The reader snapshot protocol (correctness core).** A reader reads the manifest, replays WAL frames
past its watermark, and re-reads the manifest — the standard "read metadata, read data, re-read
metadata" bracket (`read_disk_snapshot`). A concurrent **seal** on the writer moves rows buffer → new
segment, bumps the watermark, persists a new manifest, then **reclaims** (deletes) superseded WAL
segments (`Storage::seal`, `Wal::reclaim`); a reader straddling that sequence could otherwise
double-count a row (seen in both an old-manifest gap and the WAL) or drop one (WAL segment reclaimed
before the reader saw the new manifest). The bracket:

```
loop (bounded retries):
  (W,  S)  = read_manifest()          # atomic rename ⇒ complete file
  buf      = replay(wal_frames where lsn > W)
  (W2, _)  = read_manifest()
  if W2 == W: break                    # stable bracket
snapshot = S  ∪  buf                   # segments (lsn ≤ W) ∪ replayed (lsn > W)
```

Why it is exactly correct:

- **No double-count.** `S` contains precisely the rows with `lsn ≤ W`; `buf` precisely those with
  `lsn > W`. Disjoint by construction.
- **No drop.** The writer reclaims a WAL segment only *after* the new manifest is durable, and only
  for `max_lsn ≤ new_watermark`. If the bracket is stable at `W`, no seal committed a watermark past
  `W` during the read, so every frame `> W` is still present. Frame reading also tolerates a raced
  reclaim delete (`NotFound` ⇒ skip).

**Incremental tailing.** A reader reuses a `WalTailCursor` (`read_disk_snapshot_incremental`) that
tracks a per-WAL-segment read offset (always a clean frame boundary) plus a running max-LSN, so a
refresh scans only newly-appended bytes rather than re-reading the whole tail, and prunes the
now-sealed prefix (`lsn ≤ watermark`) each pass. A partially written trailing frame is ignored until
its bytes complete; the cursor resumes from the stored boundary and the next refresh picks it up. This
is cheap enough to run **on every query** (poll-on-query), keeping the no-background-threads guarantee
intact. `read_disk_snapshot` is the same protocol with a throwaway cursor — a fresh scan and an
incremental one return an identical snapshot, so the cursor is a pure performance cache, not a
behavior change.

**Refresh policy (`Refresh`).** Poll-on-query is the default (`Refresh::OnQuery`, near-real-time). A
read-only handle may instead reuse one snapshot across queries: `Refresh::Ttl(d)` rebuilds at most
every `d`; `Refresh::Manual` pins the snapshot until an explicit `Db::refresh()` (mirrored on
`BlockingDb`). The knob trades a bounded, opt-in staleness for collapsing a query burst onto a single
WAL scan; it is backed by a per-handle reader cache (the incremental cursor + the last-built table
inputs, cloned per query). `Refresh` is ignored for read-write / in-memory handles, which query their
own live buffers; the read-during-delete retry (below) force-rebuilds so a cached snapshot can never
loop on a just-unlinked segment.

**Read-during-delete (retention / compaction).** Retention and compaction delete Parquet files only
after the manifest that drops them is durable (`retain`, `compact`). A reader holding a path from an
older snapshot could open a since-unlinked file. POSIX unlink-after-open keeps the inode alive, so a
read already in progress finishes; the only gap is *path captured, not yet opened*. Mitigation
(built): the read-only query path (`collect_with_stats`) captures the snapshot's segment paths; if
`run_sql` errors **and** any of those paths has since vanished, it re-derives the snapshot from the
current manifest (which no longer lists the file) and retries, up to `READER_QUERY_TRIES` (4). A
failure with every path still present is a real error, returned as-is — detecting the race by "did a
snapshotted path disappear" (rather than by parsing error text) is robust to how DataFusion surfaces a
missing file. An optional writer-side deletion grace period (defer unlink by N seconds, or a small
tombstone generation) would make retries rare under heavy retention; not required for correctness, a
later footprint-cheap add.

**Writer exclusivity — the lockfile.** A read-write `Db::open` acquires an **exclusive advisory lock**
on `<dir>/writer.lock` (`fs4::fs_std::FileExt::try_lock_exclusive`, non-blocking;
`flock(LOCK_EX|LOCK_NB)` on POSIX / `LockFileEx` on Windows); if held, open fails fast with
`Error::lock_held`. The lock releases on handle drop / process exit (OS-cleaned even on crash — no
stale-lock cleanup logic). Readers take no lock. Footprint: zero net new crates — `fs4` is already in
the transitive tree.

**WAL-mode advertisement.** A read-write open writes a `db.info` marker advertising its WAL mode
(atomically, at open). A read-only open reads it and **rejects by default** with
`Error::reader_wal_disabled` when the writer's WAL is off (the reader could only ever see
seal-interval freshness); `DbBuilder::allow_stale_reads()` opts in. A missing/unparseable marker reads
as "unknown" and never rejects (pre-marker DBs open as before).

**Public API.** `DbBuilder::access(Access)` with `Access::{ReadWrite (default), ReadOnly}`;
`Db::open_read_only(path)` convenience. Read-write open may return `Error::lock_held`. On a read-only
`Db`, ingest/seal/maintain/retain/compact return `Error::read_only`; the query APIs work unchanged and
transparently re-derive the snapshot per query. A read-only `Storage` has `wal: None` for appends but
owns a tailer that populates the private replay buffer.

**Intra-process note.** The writer's own query path solves the analogous buffer∪segments atomicity
problem with `Storage::query_snapshot` under a single lock, plus the `Inner::sealing` staging that also
covers the off-lock segment write (§5, §7). The cross-process reader path never has that mid-seal
*drop* window — it reads manifest ∪ WAL-tail, and the WAL still holds the rows until the seal bumps the
watermark and reclaims them.

**Residual risks / open questions.**

- **Retry-on-missing bound** vs. a very aggressive retention loop: capped at `READER_QUERY_TRIES`,
  surfacing a clear error if exceeded.
- **Windows advisory-lock semantics** differ from POSIX; validate `fs4`'s behavior on supported
  platforms.
- **Manifest format** is line-based text with no checksum. Atomic rename makes torn reads impossible,
  but a truncated *rename source* on an odd filesystem is not caught; a trailing checksum line would
  harden the reader. Low priority.

Validated end-to-end by a real two-*process* integration test (`crates/imbh/tests/cross_process.rs`):
the parent re-execs the test binary as the writer and acts as the read-only reader over one temp dir,
asserting cross-process `writer.lock` rejection, page-cache WAL freshness, and no drop / no
double-count (`count(*) == count(DISTINCT)`, monotonic, terminal `== N`) across the writer's live
seals + WAL reclaims. No network or daemons (per TESTING.md).

## 8. Search: Tantivy integration

One Tantivy index per sealed segment of `logs` and `spans` (never metrics), aligned lifecycles,
no cross-segment merges. `imbh-index` is the only crate that knows Tantivy. Schema per index:

- `body`: text field, custom lightweight tokenizer (lowercase + split on non-alphanumerics, no
  stemming, no stopwords — observability tokens are identifiers, not prose). **The tokenizer is a
  standalone function**, so the row-wise `matches` fallback (§9.2) tokenizes identically and
  buffer vs. sealed results are byte-identical. Span `name` is indexed into the same `body` field,
  so one search path serves both tables.
- `service`, `severity_text` (logs): raw fields.
- `row`: u64 fast field = row ordinal within the segment's Parquet file. This is the bridge; the
  ordinal is stored as data, never assumed from doc order.
- Nothing is **stored** in Tantivy (docstore stays empty) — Parquet is the store; the index is
  purely a row-pruning accelerator.
- **`NoMergePolicy`.** Each `.tidx` is write-once (one commit per sealed/compacted segment,
  rebuilt wholesale on compaction), so the writer runs with `NoMergePolicy`: no background merge
  thread is ever spawned (honoring the no-background-threads guarantee), `Drop` is trivially clean,
  and no seal blocks on a merge.

> *Deviation:* the original design added an `attrs` JSON object field indexing all attribute maps
> for exact-term filtering. That field is **not built** — the index covers `body`/`name` text
> only; attribute filtering runs as `json_get_str` scans (§6.1).

Query bridge: `matches(col, 'query')` is compiled to a Tantivy query, executed per segment to a
sorted row-id set, and converted to a Parquet `RowSelection`. **Honest cost model:** because
segments are time-sorted, text matches scatter across pages, so the `RowSelection` skips whole
pages only when a page has zero matches and otherwise saves per-row decode CPU within a touched
page, not I/O. The provider makes this **cost-based**: it estimates hit fraction from Tantivy's
term-frequency stats and applies the `RowSelection` only below a selectivity threshold; above it,
a plain filtered scan runs (still correct — Parquet is ground truth). The `row` fast field maps a
hit to a global row ordinal, which the provider translates to `(row_group, offset)` against the
Parquet layout before building the `RowSelection`.

## 9. Query layer: DataFusion integration

`imbh-query` is the only crate that knows DataFusion.

### 9.1 Session setup

One long-lived `SessionContext` per `Db`: `target_partitions = 1` (cooperative on the caller's
runtime), a modest `batch_size` (RSS over throughput), a memory pool sized from `MemoryBudget`,
dictionary/string-view preservation on. All arrow/parquet types are consumed **via DataFusion's
re-exports** so version skew between arrow, parquet, and datafusion is impossible. DataFusion is
trimmed: `default-features = false`, enabling `parquet`, `sql`, and the datetime/string/nested/
regex expression packages + `recursive_protection`; compression, crypto/encoding/unicode
expressions, avro, and serde are off.

### 9.2 Custom `TableProvider` + pushdown contract

Each table gets one provider that unions the mutable-buffer snapshot with manifest segments.
`supports_filters_pushdown` claims:

- **Exact**: time-range predicates on the time/sort column (segment prune via manifest, row-group/
  page prune via Parquet stats).
- **Exact**: `matches(col, 'query')` on indexed tables (`logs`, `spans`) — compiled to a Tantivy
  query and applied as a cost-gated `RowSelection` (§8).
- **Unsupported** → plain filtered scan, always correct because Parquet is ground truth. This
  includes attribute equality (no Tantivy attrs field) and `service`/`severity` filters, which run
  as ordinary DataFusion filters / `json_get_str` scans.

> *Deviation:* the original design also pushed attribute-term equality and `match_all` into
> Tantivy. Neither is built — only `matches` pushes down.

### 9.3 UDFs shipped

Registered on the session: `matches(column, query)` (text search — Tantivy pushdown marker +
row-wise fallback), `json_get_str(json, key)` (attribute access over canonical JSON),
`histogram_quantile(phi, explicit_bounds, bucket_counts)` (explicit-bucket histograms), and
`hex(binary)` (id rendering). *Deviation / not built:* `match_all`, `json_get_int/float/bool`,
`rate_delta` window helper, and `span_end_time` — the plan listed these; they are not registered.

### 9.4 Typed query APIs

*Deviation:* the original design had each typed API build a `LogicalPlan` directly. In the
implementation, **every typed method composes a SQL string and calls `db.sql()`** — still one
query path, two front doors, but the consequence is that the `sql`/sqlparser frontend is **not
severable**, which is why there is no `sql` feature (§10.13).

## 10. Public API surface

The product is a **library**; any HTTP server is wiring the host owns. The typed API is
*endpoint-shaped* — it mirrors the query surfaces of Loki, Tempo, Mimir/Prometheus, SigNoz, and
VictoriaMetrics closely enough that mapping a method to a route is mechanical. `imbhd` (§10.16) is
one reference wiring. The host-integration guide is [docs/EMBEDDING.md](../../docs/EMBEDDING.md).

Load-bearing shape decisions: one `Db` handle with per-signal namespaces; **async is primary,
blocking is a facade**; SQL is lazy (`db.sql(q).collect()`), typed queries are eager
(`async fn -> Result<T>`).

**Endpoint → method quick reference** (routes the reference server or a host would map):

| Surface | Library call |
|---|---|
| Loki `query_range` (logs) | `logs().query(LogQuery)` → `LogPage` |
| Loki `index/volume` | `logs().volume(LogQuery, step)` / `logs().volume_by(LogQuery, step, &[keys])` |
| Loki labels / values | `attrs().names()`, `attrs().values(key)` |
| Tempo `traces/{id}` | `traces().get(TraceId)` → `Option<Trace>` |
| Tempo `search` | `traces().search(TraceQuery)` → `Vec<TraceSummary>` |
| Tempo/TraceQL metrics (RED) | `traces().span_metrics(SpanMetricsQuery)` → `SpanMetrics` |
| Prom `query_range` / `query` | `metrics().range(MetricQuery)` → `Matrix` / `metrics().instant(…)` → `Vector` |
| Prom histogram quantiles | `metrics().histogram_quantile(HistogramQuery)` / `exp_histogram_quantile(ExpHistogramQuery)` → `Matrix` |
| Prom `series` / `metadata` / exemplars | `metrics().series(metric)`, `metrics().catalog()`, `metrics().exemplars(metric)` |
| VM `export` / zero-copy handoff | `db.export(Table, range)` (Arrow IPC bytes) / `db.segment_files(Table)` (Parquet paths) |
| VM `status/tsdb`, force-merge, snapshot | `db.stats()`, `db.compact()`, `db.snapshot(dir)` |
| SigNoz raw SQL | `db.sql(…)` |

### 10.2 `Db` & lifecycle

```rust
pub struct Db { /* concrete struct; Send + Sync; not Clone — share via Arc<Db> */ }

impl Db {
    pub fn builder(path: impl AsRef<Path>) -> DbBuilder;   // sync open (WAL replay, manifest load)
    pub fn in_memory() -> DbBuilder;                        // ephemeral: no WAL, no on-disk segments

    // per-signal query namespaces
    pub fn logs(&self)    -> LogsApi<'_>;
    pub fn traces(&self)  -> TracesApi<'_>;
    pub fn metrics(&self) -> MetricsApi<'_>;
    pub fn attrs(&self)   -> AttrsApi<'_>;

    // ingest (async awaits acceptance; try_ never blocks / never fsyncs)
    pub async fn ingest_otlp_logs(&self,    body: &[u8]) -> Result<IngestReceipt>;  // + traces/metrics
    pub fn     try_ingest_otlp_logs(&self,  body: &[u8]) -> Result<IngestReceipt>;  // + traces/metrics
    pub async fn durable_through(&self) -> Option<Lsn>;   // None until anything is durable

    // cross-cutting / ops
    pub fn sql(&self, sql: &str) -> Query;
    pub async fn flush(&self) -> Result<()>;
    pub async fn maintain(&self) -> Result<MaintenanceReport>;
    pub async fn compact(&self) -> Result<CompactionReport>;
    pub async fn snapshot(&self, dir: impl AsRef<Path>) -> Result<SnapshotInfo>;
    pub async fn stats(&self) -> Result<DbStats>;
    pub async fn export(&self, table: Table, range: TimeRange) -> Result<Vec<u8>>;  // Arrow IPC
    pub fn segments(&self) -> Vec<SegmentRef>;
    pub fn segment_files(&self, table: Table) -> Vec<PathBuf>;                       // zero-copy handoff
    pub fn refresh(&self) -> Result<()>;   // read-only: rebuild the snapshot now (no-op for a writer)
    pub fn blocking(&self) -> BlockingDb;
    pub async fn close(&self) -> Result<()>;   // idempotent: set closed, join maintenance thread, final seal
}

pub struct DbBuilder { /* … */ }
impl DbBuilder {
    pub fn memory_budget(self, b: MemoryBudget) -> Self;   // caps buffer + query pool + writer heap
    pub fn compression(self, c: Compression) -> Self;      // Zstd(i32) | Lz4 | None
    pub fn wal(self, mode: WalMode) -> Self;               // Off | Interval(Duration) | Always
    pub fn retention(self, r: Retention) -> Self;          // Retention::days(n).max_disk_bytes(b)
    pub fn maintenance(self, m: Maintenance) -> Self;      // Manual | Background(Duration) | Runtime(Handle, Duration)
    pub fn flush(self, p: FlushPolicy) -> Self;            // when the scheduler seals: periodic / bytes / rows / WAL / idle
    pub fn refresh(self, r: Refresh) -> Self;              // read-only freshness: OnQuery | Ttl(Duration) | Manual
    pub fn open(self) -> Result<Arc<Db>>;         // handles are shared as Arc<Db>
    pub async fn open_async(self) -> Result<Arc<Db>>;
}
```

> *Deviations vs the original design:* `DbBuilder` has `promote()` (§6.1) but no `signals()` or
> `runtime()`; `export` takes one `Table` and returns `Vec<u8>` (not a stream over `&[Table]`);
> `segments()` takes no arguments; and there is no `db.resources()`.

**Flush policy.** `FlushPolicy` (in `imbh-core::config`) is the *when* half of the scheduler pair
described in §5; `Maintenance` is the *who*. Its triggers OR together and each is independently
optional:

| Strategy | Builder | Fires when |
|----------|---------|-----------|
| periodic | `FlushPolicy::periodic(d)` / `.every(d)` | `d` has passed since the last seal |
| size (heap) | `.at_buffer_bytes(n)` / `.size(FlushSize::{Budget,Bytes,Off})` | buffered heap ≥ `n` (default: the memory-budget-derived `seal_threshold_bytes`) |
| size (rows) | `.at_buffer_rows(n)` | buffered rows across all tables ≥ `n` |
| WAL size | `.at_wal_bytes(n)` | the on-disk WAL ≥ `n` (sealing is what lets it be reclaimed) |
| idle | `.after_idle(d)` | nothing ingested for `d` and the buffer is non-empty |

`.tick(d)` sets the evaluation cadence (default 1s, clamped to [5ms, 60s]) and throttles the only
measurement that costs anything — one lock for the buffer gauges, plus a directory scan when a
WAL-size trigger is configured. The loop still *sleeps* in ≤1s slices whatever the tick, so `close()`
never waits a full tick. `FlushPolicy::manual()` disables every trigger (retention and the WAL fsync
timer still run); leaving `DbBuilder::flush` unset resolves to the default policy with the
`Maintenance` interval as its seal cadence, which is the pre-`FlushPolicy` behavior. A policy also
parses from a spec string (`"interval=5s,wal=64MiB"`, or `"manual"`) via `FromStr`, which is how
`imbhd` exposes it as one environment variable (§10.16).

`Db` is a **concrete, non-`Clone`** `Send + Sync` struct; `open()` / `open_read_only()` return an
`Arc<Db>` and you share that `Arc` across the app (the DB owns no second internal refcount — consumers
already hold it inside an `Arc`). The typed query namespaces (`logs()`/`traces()`/`metrics()`/`attrs()`),
`sql()`, `export()`, and `blocking()` take `self: &Arc<Self>` so the returned `'static` handle keeps an
owned share; the direct ops (`ingest_*`, `flush`, `compact`, `stats`, `close`, …) are `&self` and reach
through the `Arc`. Dropping the last `Arc<Db>` without `close()` is safe — the WAL recovers on next open.

### 10.3 Error model

`Result<T> = std::result::Result<T, Error>`, with `Error` categories for open/ingest/query/
storage/config plus a terminal `Closed` (returned by every op after `close()`). The reference
server maps errors to HTTP status via error-classification helpers.

### 10.4 Shared vocabulary

- **Time.** `Timestamp` (i64 epoch-nanos, UTC; serializes as a decimal string — the OTLP-JSON /
  Loki convention that dodges the 2^53 precision loss on nanos), `DurationNs` (u64 nanoseconds),
  `TimeRange` (`between` / `since` / `all`, `.step(d)` for bucketing), `Direction`
  (logs default newest-first).
- **Attribute matching.** *Deviation:* rather than a single `AttrMatcher`/`MatchOp` vocabulary
  type, each query builder exposes matcher **methods** directly:
  `attr_eq`, `attr_exists`, `attr_matches` (term search), `attr_in`, `attr_not_in`,
  `attr_gt`/`attr_ge`/`attr_lt`/`attr_le`, `attr_regex`. `service.name` (and `service`) hits the
  built-in `service` column and a configured `promote` key its dictionary column (§6.1); everything
  else resolves via `json_get_str` (there is no attrs Tantivy index, §8). The same resolution backs
  group-by, so `service.name` is groupable as well as filterable.
- **Ids & enums.** `TraceId([u8;16])` / `SpanId([u8;8])` (lowercase hex), `Lsn` (a
  `NonZero<u64>` type alias — 0 is never a valid LSN, so "nothing durable / not yet written" is
  `Option<Lsn>` = `None` rather than an in-band `Lsn(0)` sentinel),
  `SeverityNumber(u8)`, and the `Table` enum: `Logs`, `Spans`, `MetricsGauge`, `MetricsSum`,
  `MetricsHistogram`, `MetricsExpHistogram`, `MetricsSummary`.
- **Values.** OTel `AnyValue` is the value model in `imbh-core`, serde-free by default (no
  `serde_json` in every build; canonical JSON is handled by `imbh-core::json`); row DTOs carry owned
  `Attributes` (key-ordered `(String, AnyValue)` pairs) with `get`/`get_str`/`iter`. `Serialize`/
  `Deserialize` are derived only under the optional `serde` feature (§10.13).

### 10.5 Ingest

`ingest_otlp_{logs,traces,metrics}` decode protobuf OTLP/HTTP bodies (uncompressed), append to
the buffer + WAL, and await *acceptance*; under `WalMode::Always` the awaiting path also fsyncs
(`receipt.durable == true`). `try_ingest_*` never blocks and never fsyncs. For an inline receipt,
confirm durability with `durable_through().await >= receipt.lsn` (both `Option<Lsn>`) or force it with
`flush()`; a queued receipt has `lsn == None` (`is_queued()`), so use `flush()` / `close()`.

```rust
pub struct IngestReceipt {
    pub accepted: u64,        // records parsed and appended
    pub rejected: u64,        // per-point drops
    pub lsn: Option<Lsn>,     // Some(assigned lsn) inline; None while queued for the async worker
    pub durable: bool,        // fsync'd before return (only the awaiting path under WalMode::Always)
}
impl IngestReceipt {
    pub fn is_queued(&self) -> bool { self.lsn.is_none() }   // enqueued for Ingest::Async, not written
}
```

**Async ingest (opt-in, §5).** With `Ingest::Async { handle, capacity, overflow }` the decode still
runs on the caller (so `accepted` and a malformed-body error are still synchronous), then the WAL +
buffer write is handed to one background worker task. The receipt is then a *queued acknowledgement*
— `is_queued()` is true, `accepted` is real, but `lsn == None` and `durable == false`, so the per-call
`durable_through() >= receipt.lsn` handshake does not apply; confirm durability globally with
`flush()`/`close()`. The worker drains the queue in bursts: it appends each job's WAL frame with
`sync_now = false`, then calls `Storage::group_commit()` **once per burst** — a single fsync that
makes the whole batch durable and advances `durable_through` to the highest appended LSN. So
`WalMode::Always` durability is preserved (the fsync moves off the caller *and* is amortized across
the burst rather than paid per job); `Interval`/`Off` are unaffected (they never fsync per-append, and
`group_commit` is a no-op for them). The bounded queue overflow policy is:

- `Overflow::Block` — the async `ingest_otlp_*` call awaits a free slot (natural backpressure); the
  non-blocking `try_ingest_otlp_*` cannot await and so fails fast when full.
- `Overflow::Fail` — return a backpressure `Error::queue_full` (`is_backpressure()`) when full.
- `Overflow::DropOldest` — evict the oldest un-processed job, then enqueue (load-shed; counted in
  `stats().ingest_dropped`).

`stats()` also exposes `ingest_queue_depth` and `ingest_errors` (worker-side WAL/buffer failures, which
have no caller to return to). A clean `close()` drains the queue before the final seal; dropping the
last `Db` handle without `close()` discards any in-flight queued jobs (same `Weak` lifecycle as the
maintenance worker).

> *Deviation:* there is no `partial_errors` field. `rejected` is `0` under the default duplicate
> policy; it becomes non-zero under `Duplicates::Reject` (below), which is also what populates the
> OTLP/gRPC `partial_success.rejected_data_points` on the metrics export.

#### 10.5.1 Duplicate metric timestamps

Two metric datapoints sharing a series **and** a timestamp have no PromQL meaning — its series
identity is `service` + `__name__` + the string attributes, and there is no rule for two values at one
instant. `Duplicates` (in `imbh-core::config`, set via `DbBuilder::duplicates`, `IMBH_DUPLICATES` on
`imbhd`) picks which end of the pipeline says so:

- `ErrorOnRead` (default) — ingest takes everything; a PromQL query that materializes such a series
  fails with `SemanticError::DuplicateTimestamp`, naming the metric, the label set and the instant,
  and carrying `dto::KIND_DUPLICATE_TIMESTAMP` over the head wire.
- `LastWins` — the duplicated instant collapses to one point at read time, so a bad point degrades one
  datapoint instead of the whole metric. The survivor is chosen by a **total order on the value** (any
  real number outranks NaN, then `f64::total_cmp`; for histograms, greatest total bucket count then
  the bucket vector and boundaries), never by scan order: metric segments carry no ingest-sequence
  column and the read SQL orders by time alone, so a positional rule would let two identical queries
  disagree after a flush or compaction. The collapse is a pure function of the fetched sample
  multiset. This is the only remedy for data already written.
- `Reject { recent }` — ingest drops a point whose `(series, timestamp)` is already among the last
  `recent` accepted, counting it in `IngestReceipt::rejected`. Reads stay as strict as the default,
  since points written before it was enabled are still there.

The guard (`imbh/src/dedup.rs`, `#[cfg(feature = "ingest")]`, `std` only) is a two-generation set of
`(series_hash_128, timestamp)` keys, preallocated so it never rehashes: fixed ~13 MB at the default
`recent = 262144`, and nothing at all under the other two policies. It runs at the **decode** site,
above `Storage`, shared by the inline path, the async decode and WAL replay — which is what lets the
async path report an exact `rejected` count, since the queued receipt returns before the worker runs.

A per-series `last_timestamp` rule was rejected: it is order-sensitive, and since the WAL stores the
raw body and replay starts with an empty guard, it could reject on replay a point the writer had
accepted. The set rule is order-commutative, so `G_replay ⊆ G_original` holds at every replayed
record and **replay is strictly more permissive** — it can never drop a row the writer kept. It also
leaves out-of-order and late-arriving data accepted, as §7 has always allowed.

Consequently the guard is best-effort by design and does **not** catch: out-of-order points; duplicates
older than `recent`; duplicates straddling a restart (the guard starts empty at every open); points
differing only in a non-string attribute (the key is the byte-exact canonical `attributes` blob, while
PromQL lifts only the string entries); or a metric name emitted as both a gauge and a sum, which is
kept on separate discriminants so a legitimate sum is never dropped because of a same-named gauge — an
instant selector unions those two tables, so that one is a *structural* duplicate the read side
resolves. Which of two differing values survives is also not stable across a restart.

The typed `MetricsApi::range`/`instant` path resolves duplicates the same way, and by the same rule.
Under `LastWins` the scan is wrapped in a `ROW_NUMBER() OVER (PARTITION BY <row identity>
ORDER BY isnan(value) ASC, value DESC)` subquery keeping rank 1; under every other policy the SQL is
byte-identical to what it was before, and the `WHERE` stays on the inner scan either way so the §9.2
pushdown contract is untouched. `instant` and `range_batches` inherit it for free.

Two things about that ordering are easy to get wrong and are pinned by tests:

- **The partition key is row identity, not PromQL label-set identity**, and must include `resource`
  and `scope`. `k8s.pod.name` / `host.name` live in the resource, so N replicas emitting the same
  counter at the same instant differ in nothing else — partitioning on `(metric, service, attributes,
  time)` alone would collapse a legitimate N-way `sum` to 1, turning a modest over-count into a large
  *under*-count on exactly the counters people alert on. Promoted attribute columns are deliberately
  absent from the key: they are projections of the `attributes` JSON, which already discriminates them.
- **NaN demotion needs `isnan(value)`, not a comparison.** DataFusion 54 orders floats by a *total*
  order rather than IEEE semantics, so `value = value`, `value >= value` and the
  `< 0 OR > 0 OR = 0` idiom all report **true** for NaN — a NaN would silently win every duplicate.
  `isnan` is available despite the workspace's `default-features = false` pin, because DataFusion
  declares `datafusion-functions` as a non-optional dependency with default features on; it costs zero
  crates. The corollary is that plain `value DESC` already matches `duplicate_value_cmp`'s second
  clause, since DataFusion's float sort *is* `total_cmp`.

> Remaining asymmetry, deliberate: `ErrorOnRead` (the default) does not *fail* on a duplicate in the
> typed path the way PromQL does. Parity would need a second detection scan on every typed range query
> for every user, and would turn queries that return a number today into errors on published crates.
> The asymmetry is also narrower than it looks — a duplicated instant has no PromQL meaning, whereas
> the typed API is a SQL aggregation builder where `SUM` over two rows is well-defined, just not what
> the caller wanted. If parity is wanted later, the cheap form is a warning surfaced via `QueryStats`.

### 10.6 Logs API

```rust
impl LogsApi<'_> {
    pub async fn query(&self, q: LogQuery) -> Result<LogPage>;
    pub async fn volume(&self, filter: LogQuery, step: Duration) -> Result<Vec<VolumeBucket>>;
    pub async fn volume_by(&self, filter: LogQuery, step: Duration, keys: &[&str]) -> Result<…>;
    pub async fn count(&self, filter: LogQuery) -> Result<u64>;
    pub async fn query_batches(&self, q: LogQuery) -> Result<Vec<RecordBatch>>;
}
```

`query_batches` is the raw-Arrow twin of `query`: same SQL, but it stops before row-DTO
materialization and hands back the `RecordBatch`es. It is **not** feature-gated — the `imbh-lgtm`
source adapter reads logs through it (§10.18) — unlike the stats-returning
`query_batches_with_stats` under the `proto` feature (§10.17).

`LogQuery` is built with `new()` + `service`, `severity_at_least`, `matches` (full-text), the
`attr_*` matchers (§10.4), `trace_id`/`span_id` (raw-binary correlation equality — the trace→log
drill-down partner of `traces().get`, bloom-prunable like §10.7), `range`/`since`, `limit`,
`direction`, `observed_after(Timestamp)` and `order_by(LogOrder)` (§10.6.1), and `after(cursor)` for
paging. `LogPage` carries the entries and a next-page cursor.

#### 10.6.1 The two clocks, and why follow mode uses the second one

Every log row carries two instants: `time`, when the producer says the event happened, and
`observed_time`, when it was captured. `LogOrder { Time (default), ObservedTime }` and
`observed_after` expose the second as a filter and sort axis; the `SELECT` projection is unchanged.

This is what closes the Docker log driver's `--tail 0 -f` race. Event time is *not* monotone in
arrival — ingest lands a line up to one batch interval after the container emitted it — so a watermark
of `Timestamp::now()` on the event clock skips a line emitted just before the follow started. The
driver now watermarks and follows on `observed_time`, which for that driver is dockerd's capture stamp
and is monotone in arrival, while `--tail N` and full history stay ordered by **event** time because
that is what `docker logs` prints. Only the cursor moved. Neither clock is monotone in the other, so
paged watermarks merge by max on each clock independently rather than taking the last seen.

No schema column was added. An earlier framing of this problem, and of the metrics duplicate collapse
above, proposed an ingest-sequence column; both were closed without one, and §10.5.1 explains why the
metrics half must *not* use scan order even if such a column existed.

The residues are real and documented in `docs/DOCKER_LOG_DRIVER.md` rather than papered over: a VRL
script can overwrite `.observed_timestamp` and break the cursor's monotonicity; an exact nanosecond
tie is still resolved once by the strict `>`; and `--tail 0` has no uniquely correct answer at all,
since json-file's "seek to the end of the file" is defined by what is *durably recorded* while imbh's
batching means some already-emitted lines are not yet recorded. Arrival order makes the cut stable and
explainable, not provably right.

> *Deviations:* `PageCursor` is a numeric **offset**, not the `(timestamp, segment, ordinal)`
> keyset the design specified (paging is not the immutable, tie-safe cursor originally intended);
> `LogPage`'s query-stats counters are not populated; and there is **no `tail` or `query_stream`**
> (live/streaming log APIs are not built).

### 10.7 Traces API

```rust
impl TracesApi<'_> {
    pub async fn get(&self, id: TraceId) -> Result<Option<Trace>>;         // None → 404
    pub async fn search(&self, q: TraceQuery) -> Result<Vec<TraceSummary>>;
    pub async fn span_metrics(&self, q: SpanMetricsQuery) -> Result<SpanMetrics>;  // RED rollups
}
```

`TraceQuery` builds with `service`, `name`, `kind`, `status`, `min_duration`/`max_duration`,
`matches`, the `attr_*` matchers, `range`/`since`, `limit`. `SpanMetricsQuery` reuses that filter
plus `group_by(key)` and `step`.

> *Deviations:* `Trace` is a flat span list — no `roots`, no `SpanTree`/`.tree()` waterfall
> assembly. `TraceSummary` is flat — no `span_sets`/`SpanSet`/`SpanSummary` and no
> `spans_per_trace`. `span_metrics` returns a bespoke `SpanMetrics` struct, not `Matrix`, and there
> is no `SpanMetric` selector enum (`Rate`/`ErrorRate`/`Duration(q)`).

### 10.8 Metrics API

```rust
impl MetricsApi<'_> {
    pub async fn catalog(&self) -> Result<Vec<MetricMeta>>;                 // name, kind, unit
    pub async fn range(&self, q: MetricQuery) -> Result<Matrix>;            // Prometheus range
    pub async fn instant(&self, q: MetricQuery) -> Result<Vector>;          // Prometheus instant
    pub async fn histogram_quantile(&self, q: HistogramQuery) -> Result<Matrix>;
    pub async fn exp_histogram_quantile(&self, q: ExpHistogramQuery) -> Result<Matrix>;
    pub async fn series(&self, metric: &str) -> Result<Vec<Attributes>>;    // distinct label sets
    pub async fn exemplars(&self, metric: &str) -> Result<Vec<Exemplar>>;   // trace links
}
```

`MetricQuery` is built from a kind constructor — `MetricQuery::gauge("cpu")` /
`MetricQuery::sum("requests")` — plus `aggregation`, `group_by`, `filter`/`filter_ne`/
`filter_regex`/`filter_not_regex`, `range`/`since`, `step`, and `.rate()` (per-second over delta) /
`.rate_counter()` (over cumulative). `HistogramQuery`/`ExpHistogramQuery` add `.quantile(phi)`.
`Matrix`/`Vector` are Prometheus-shaped (`metric` label map + `[secs, "value"]` samples, the
string encoding letting NaN/±Inf survive).

> *Deviation:* the original single `MetricQuery { selector, rate: Rate, time_agg: TimeAgg,
> space_agg: SpaceAgg, … }` with the `Rate`/`TimeAgg`/`SpaceAgg` enums is replaced by the
> constructor + builder-method style above.

### 10.9 Attribute discovery

```rust
impl AttrsApi<'_> {
    pub async fn names(&self) -> Result<Vec<String>>;          // distinct attribute keys
    pub async fn values(&self, key: &str) -> Result<Vec<String>>;
}
```

> *Deviation:* simpler than the scoped design — no `Signal`/`AttrScope`/`TimeRange`/filter
> parameters and no typed `AttrName`/`AttrValue` results.

### 10.10 SQL escape hatch

`db.sql(&str) -> Query`; `Query::collect().await -> Vec<RecordBatch>` (via `imbh::arrow`
re-exports). Tables: `logs`, `spans`, `metrics_{gauge,sum,histogram,exp_histogram,summary}`. UDFs
`matches`, `json_get_str`, `histogram_quantile`, `hex` are registered (§9.3). This is the same
query path the typed APIs build on — no second engine.

### 10.11 Ops & admin

`export(Table, range)` returns Arrow-IPC bytes; `segment_files(Table)` hands back live immutable
Parquet paths (safe to read concurrently — segments never mutate) for zero-copy handoff to
DuckDB/pandas/polars; `segments()` returns `SegmentRef`s (table, path, time range, rows, bytes);
`compact()`, `snapshot(dir)` (manifest copy + hard-linked segments), `stats()` (per-table totals +
buffer/WAL/durable-LSN), `maintain()`, `flush()`, `durable_through()`. No delete-series API by
design.

### 10.12 Async, blocking, streaming

Async is the native surface (a query is driven by whoever awaits it — no hidden execution pool,
which is how the no-background-threads guarantee holds for query execution). The **blocking
facade** (`db.blocking() -> BlockingDb`) owns a private current-thread runtime and exposes
`ingest_otlp_*`, `sql`, `flush`, `maintain`, `compact`, `snapshot`, `stats`, `export`, `close`.

`Query::collect` materializes the whole result; `Query::stream` (and `stream_with_stats`, which also
returns a `StreamStatsHandle` for the read-side counters) returns a **bounded-memory**
`SendableRecordBatchStream`. The scan is genuinely lazy — the custom `SegmentTableProvider` hands a
`PartitionStream` to `StreamingTableExec` that reads one Parquet batch per `poll_next` (never a
collect-then-`MemTable`), so the executor yields after each batch. Two residual quanta are bounded and
expected: a pipeline-breaker (`ORDER BY`, hash-aggregate, `DISTINCT`) drains its input before the
first output batch, and a cold segment's `std::fs` read blocks for that read. The streamed snapshot is
pinned at plan time (no read-during-delete retry — the caller must not unlink streamed segments until
the stream is drained/dropped).

> *Deviations:* `BlockingDb` does **not** expose the typed query namespaces
> (`logs()`/`traces()`/`metrics()`) — sync hosts use `blocking().sql(…)` for queries. Blocking
> `ingest_otlp_*` calls the non-fsync `try_` path, so a blocking ingest receipt is **never
> `durable`** even under `WalMode::Always` (use `flush()` to force durability). There are no
> streaming/`tail` iterators.

### 10.13 Feature gating

*Deviation:* two cargo features exist on the `imbh` facade: **`search`** (default on) and
**`serde`** (default off). Turning `search` off drops the whole Tantivy subtree (~59 crates);
`matches()` then falls back to a full tokenized scan (identical results, no pruning), and
promoted-column filters are unaffected. The **`serde`** feature derives `Serialize`/`Deserialize`
for the typed query builders (`LogQuery`/`TraceQuery`/`MetricQuery`/`HistogramQuery`/
`ExpHistogramQuery`/`SpanMetricsQuery` and their operator enums) and the result DTOs
(`LogEntry`/`LogPage`/`QueryStats`/`VolumeBucket`, `Trace`/`TraceSummary`/`Span`/`SpanMetrics*`,
`Matrix`/`Vector`/`Sample`/`MetricSeries`/`MetricMeta`/`Exemplar`); it forwards to `imbh-core/serde`
so the embedded domain types serialize too (`TraceId`/`SpanId` as lowercase-hex strings). Enabling
it adds no new crate to the graph — serde is already compiled transitively. The **`proto`** feature
(default off) is the query-binding surface (§10.17): it pulls the first-party `imbh-proto` crate
(protobuf wire types for the query inputs) plus `TryFrom` mappings and Arrow-`RecordBatch` query
entry points; it adds no new *runtime* third-party crate (prost is already compiled; protox/prost-build
are build-time only); `proto` implies `query` (its `TryFrom` mappings target the typed query builders).

**Producer / consumer split (the primary footprint lever).** The `imbh` facade carries two role
features, both default-on: **`ingest`** (producer — OTLP decode via `imbh-otlp` + the `Db::ingest_otlp_*`
write paths) and **`query`** (consumer — the DataFusion query engine via `imbh-query`/`datafusion` + the
`sql`/`logs()`/`traces()`/`metrics()`/`attrs()`/`export` surface). `search` implies `query` (it is a
query-side accelerator). Storage-level ops (`open`/`stats`/`compact`/`maintain`/`snapshot`/
`segment_files`) need neither and stay in every build. A host compiles only the role it plays; a
**producer** build (`--no-default-features --features ingest`) drops the entire DataFusion + sqlparser +
tantivy subtree, a **consumer** build (`--features query`) drops the OTLP decoder. Measured unique
crates (`cargo tree -e no-dev -p imbh`): **default 287 → producer 104 (−64%) / consumer 221 /
storage-only 80**. This is enabled by imbh-storage depending on `arrow`+`parquet` *directly* (not via
`datafusion::{arrow,parquet}` re-exports), so the storage/ingest paths carry Arrow types without pulling
the query engine; the workspace pins one arrow/parquet 58.3.0 (datafusion 54's exact versions) so the
tree still unifies to a single arrow. The CI `feature-matrix` job builds + clippy-checks the reduced
configs and asserts the cuts via `cargo tree`. **Consequence:** a *pure* consumer (`query` without
`ingest`) reads **sealed segments only** — WAL-tail replay is `ingest`-gated because WAL frames store raw
OTLP bytes that only the imbh-otlp decoder can replay (§7); a host wanting near-real-time cross-process
reads keeps `ingest` on (the default has both). Per-signal `logs`/`traces`/`metrics` gates were
prototyped then dropped in favor of this axis; `sql` off is not implemented (`sql` is not severable —
the typed APIs build SQL strings, §9.4); `zstd`/`mimalloc` gates give no user-facing win.

### 10.14 Stability

Pre-1.0 the surface may change. Response structs and public enums are `#[non_exhaustive]`.
Arrow/Parquet types reach users only through `imbh::arrow` / `imbh::parquet` re-exports (the facade
depends on `arrow`/`parquet` directly, workspace-pinned to datafusion 54's exact 58.3.0 so the tree
unifies to a single arrow — this is what lets a producer build re-export Arrow types with no DataFusion).

### 10.15 Worked examples

```rust
use imbh::{Db, FlushPolicy, MemoryBudget, Retention, WalMode, Maintenance};
use std::time::Duration;

// Open (durable, on disk). Or Db::in_memory() for an ephemeral, WAL-less DB.
let db = Db::builder("./telemetry")
    .memory_budget(MemoryBudget::total(128 << 20))
    .retention(Retention::days(7).max_disk_bytes(20 << 30))
    .wal(WalMode::Interval(Duration::from_secs(1)))
    .maintenance(Maintenance::Background(Duration::from_secs(30)))  // who schedules + retention cadence
    .flush(FlushPolicy::periodic(Duration::from_secs(5))            // when to seal: any trigger fires
        .at_wal_bytes(64 << 20)
        .after_idle(Duration::from_secs(2)))
    .open()?;

db.ingest_otlp_logs(&otlp_bytes).await?;               // ExportLogsServiceRequest protobuf

// Logs: service + full-text + attribute equality, newest first.
let page = db.logs().query(
    imbh::LogQuery::new().service("checkout").matches("timeout error")
        .attr_eq("peer.service", "cart").since(Duration::from_secs(900)).limit(100),
).await?;

// Traces: slow spans, then the full trace.
let slow = db.traces().search(
    imbh::TraceQuery::new().service("checkout").min_duration(Duration::from_millis(500)),
).await?;
if let Some(t) = slow.first() { let trace = db.traces().get(t.trace_id).await?; }

// Metrics: p99 by route (histogram), 60s steps.
let p95 = db.metrics().histogram_quantile(
    imbh::HistogramQuery::new("http.server.duration").quantile(0.95)
        .group_by("http.route").step(Duration::from_secs(60)),
).await?;

// Cross-signal SQL (lazy).
let rows = db.sql(
    "SELECT service, count(*) FROM logs WHERE matches(body, 'error') GROUP BY service"
).collect().await?;

// Sync host: the blocking facade owns a private runtime (queries via SQL).
let b = db.blocking();
b.ingest_otlp_logs(&otlp_bytes)?;
let batches = b.sql("SELECT count(*) FROM logs")?;
```

Server wiring is a thin adapter: decode the request, call the matching `db` method, encode the
result — see §10.16 and `docs/EMBEDDING.md`.

### 10.16 Reference server

`imbh-server` / `imbhd` is a worked example, not the product: an HTTP/1.1 server on **axum over
hyper** exposing OTLP/HTTP ingest on `/v1/{logs,traces,metrics}` (protobuf, `Content-Encoding: gzip`
accepted), a SQL query endpoint `POST /api/query` (JSON rows or Arrow IPC out), an MCP endpoint
`POST /mcp` (§10.16.1), `GET /stats`, admin `POST /admin/{flush,compact}`, and `GET /health`.

**Why a framework here does not cost the footprint claim.** The §11 crate budget is written against
the *library* graph, and `scripts/footprint-gate.sh` measures exactly that (`cargo tree -p imbh`).
The dependency direction is `imbh ← imbh-server`, so nothing this crate links is in that number: the
facade sits at 275 crates with or without axum. `imbh-server` is optional and adds ~17 crates to its
own graph; the release `imbhd` binary grew ~1.4 MiB (31.2 → 32.6 MiB) against a 42 MB target. The
`grpc` feature got *cheaper*, not dearer — tonic 0.14 routes through axum, so hyper/tower/axum used
to arrive with it and are now already present; `grpc` adds only tonic and h2 on top, and the
full-feature graph is unchanged at 310 crates.

Handlers run on one shared multi-threaded runtime, and every `Db` call goes through `offload`, which
runs it under `tokio::task::block_in_place`. That is not incidental: `Db`'s futures do **blocking**
parquet/tantivy I/O inside themselves (there is no `spawn_blocking` anywhere in the library), so
awaiting one on a runtime worker would park it and starve every other connection. Offloading moves
the bound from socket count to the blocking pool — i.e. onto actual work, which is the right axis
once connections are tasks rather than threads. The route table is a plain `axum::Router`, exposed as
`imbh_server::app(db)` so a host that already runs axum can mount imbh's endpoints directly rather
than adopt `serve()`'s opinions about runtimes, ports, and shutdown.

OTLP/gRPC ingest is available behind the **optional, off-by-default `grpc` feature** (`cargo build -p
imbh-server --features grpc`). Since gRPC is HTTP/2 + protobuf framing that the HTTP/1.1 listener
does not speak, the feature pulls **tonic** and serves the three OTLP collector services
(`LogsService` / `TraceService` / `MetricsService`) on a second port (default `127.0.0.1:4317`,
alongside HTTP on `4318`). Both share one `Arc<Db>`; each gRPC `export` re-encodes the decoded request
and funnels into the same `Db::ingest_otlp_*` path the HTTP routes use, so there is one ingest and
validation story. Errors map to gRPC status via the §10.3 classifiers (not-found → `NotFound`, user
error → `InvalidArgument`, else `Internal`), mirroring the HTTP status mapping. The whole tonic/hyper
subtree used to be confined to that feature; now that the HTTP listener brings hyper/tower/axum
anyway, what `grpc` adds on top is just tonic and h2. The **default build still carries no gRPC
transport**.

#### 10.16.1 MCP server (both transports)

imbh serves the **Model Context Protocol** over **both** transports MCP defines, so an agent can
search logs, pull traces, and query metrics through the same process that holds them — no Grafana,
no datasource proxy, no export step:

- **Streamable HTTP** — `imbhd` at `POST /mcp` (`imbh-server`), on in the default build.
- **stdio** — `imbh-tui --mcp-stdio`, newline-delimited JSON-RPC on stdin/stdout. stdio is the
  transport clients "SHOULD support whenever possible" and the one an agent that *spawns* its server
  speaks; it binds no port, and the pipe is the authorization.

The protocol lives in its own crate, **`imbh-mcp`** (§12), because both transports need it and the
dependency direction forbids `imbh-tui` reaching into `imbh-server`. Its dispatch is
transport-agnostic — `handle(db, bytes, &Transport) -> Reply`, bytes plus which transport in, a
`Value` body out — and `Transport` exists for exactly one reason: the stateless revision's
header mirror (`MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name`) is a rule of the *HTTP* transport,
so `Transport::Stdio` validates none of it and serves a modern request on its `_meta` alone. The
crate also owns the two JSON serializers `POST /api/query` and `GET /stats` share with the
`query_sql` and `db_stats` tools, so the tool surface and the HTTP surface cannot describe the same
rows — or the same database — two different ways. `stats_json` takes that one step further: it
converts `DbStats` into the head API's `imbh_head::dto::Stats` (§10.19) and serializes *that* derive,
so `GET /stats`, `db_stats` and `GET /api/head/stats` are literally one serializer — the plain
endpoint reports the ingest gauges, its body deserializes into the typed value, and a `None` durable
LSN is `null` rather than the `0` that no `Lsn` can be. The `dto`-only edge to `imbh-head` adds no
third-party crate to any graph (`imbh-mcp` 283 → 284, all of it the workspace crate itself).

Neither transport adds **any crate** to any graph. The protocol speaks JSON-RPC through `serde_json`
and Base64 (the HTTP transport's `=?base64?…?=` header sentinel, and `AnyValue::Bytes` attributes)
through `base64` — both already compiled in the default tree, `serde_json` via `arrow-json` and
`base64` via `arrow-cast`, both under DataFusion. Measured 275 → 275 on the facade, and lifting the
module out of `imbh-server` moved both consumers by exactly one *workspace* crate (`imbh-server`
300 → 301, `imbh-tui` 312 → 313 third-party-plus-workspace edges). The stdio transport's `--url`
forwarding mode (below) is likewise dependency-free: one buffered `POST` per message over
`std::net::TcpStream`, hand-written, rather than an HTTP client subtree in the TUI binary. Note that
`serde_json` here is the *default* build without `preserve_order`, so object keys serialize
alphabetically; turning that feature on would flip it for every `serde_json` user in the graph,
DataFusion included, to buy nothing but field order.

A stdio session gets its answers from one of two backends. `--mcp-stdio <dir>` opens the directory
with `Db::open_read_only`, which takes no writer lock and therefore reads *alongside* a running
`imbhd` — the common case, and it needs nothing running at all. What a read-only opener cannot see
is the writer's unsealed buffer, so `--mcp-stdio --url <addr>` instead forwards each message to that
daemon's `POST /mcp`, synthesizing the header mirror from the body it is forwarding (the daemon
enforces the agreement the pipe could not carry). The loop is strictly one message at a time: a `Db`
query is blocking parquet/tantivy I/O from start to finish, so concurrent requests would contend for
the same disk rather than overlap, and the blocking `std::io` handles are sound precisely because
nothing else shares that runtime.

The 15 tools are **read-only** wrappers over §10.5–§10.9: `db_stats`, `list_attribute_keys` /
`list_attribute_values`, `search_logs` / `count_logs` / `log_volume`, `search_traces` / `get_trace` /
`span_metrics`, `list_metrics` / `metric_series` / `query_metric_range` / `query_metric_instant` /
`histogram_quantile`, and `query_sql`. Nothing ingests, flushes, compacts, or applies retention, so
the endpoint is safe to hand an agent that is only meant to *look*. Every tool answers with one JSON
document in a `text` content block; argument and query failures come back as tool-execution errors
(`isError: true`) rather than JSON-RPC errors, which is what lets a model self-correct rather than
see a transport failure. Time windows default to a 1h look-back with `since` / explicit
`start_unix_nano`/`end_unix_nano` overrides — which is why the tool descriptions and `instructions`
point a model at `db_stats` first, since a replayed or historical DB holds nothing in the last hour.

The tool surface is **dual-era**, because MCP revision `2026-07-28` made the protocol stateless (no
`initialize`; each request declares its version in `params._meta`; `server/discover` reports
capabilities; results carry `resultType: "complete"`) while `2025-11-25` and earlier open with the
`initialize` handshake. Serving only one would be unusable by half the clients in the field, so era
is chosen **per request**: a `params._meta` protocol version — or a `server/discover` call — selects
the stateless path, anything else the handshake path. On the stateless path *over HTTP* the
`MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` headers are validated against the body (`-32020` on
mismatch, including the `=?base64?…?=` sentinel decode) and an unimplemented version is refused with
`-32022` listing what is supported. Nothing streams, so every response is a single JSON document and
no session id is minted; `GET`/`DELETE /mcp` answer `405`, which is what the spec prescribes for a
server offering neither stream nor session. Over stdio the same rules hold minus the headers, and
framing takes their place: one line in, at most one line out, a notification answered with nothing, a
blank line skipped, and a malformed line answered with a parse error that does not end the session.

Exposure over HTTP follows the rest of `imbhd`: unauthenticated, gated by whatever fronts it. The one
defence implemented here is the transport's DNS-rebinding rule — a request carrying a browser `Origin`
outside the loopback set is refused `403`, so a page the user merely visits cannot drive the tools on
their loopback `imbhd`. `IMBH_MCP_ALLOWED_ORIGINS` (comma-separated, `*` to disable) widens it. Over stdio there is no
equivalent question: no port is bound, and only the process that spawned the session can write to its
pipe. See `docs/MCP.md` for client configuration on both transports.

A **Docker logging-driver plugin** is available behind the optional, off-by-default `docker` feature
(`cargo build -p imbh-server --features docker`), turning `imbhd` into a `docker.logdriver/1.0`
plugin: `--log-driver imbh` writes a container's stdout/stderr straight into the embedded `Db`. It is
Unix-only (`#[cfg(unix)]`) and adds **no crate** to the graph — the plugin API is HTTP/1.1 over
`AF_UNIX` on the same axum/hyper stack as the TCP listener, sharing its request handling and so its
body limits, deadlines, and decoding), its JSON goes through
`imbh::parse_json`, and its wire format (Docker's length-prefixed `logdriver.LogEntry` frames) is
declared with prost's derive rather than generated, so it rides on the prost + opentelemetry-proto
message types already present via `imbh-otlp`. All five endpoints are implemented
(`Plugin.Activate`, `LogDriver.{StartLogging,StopLogging,Capabilities,ReadLogs}`).

**Remapping** (`docker-remap`, also off by default, but enabled in the published plugin) inserts a
VRL program between the reassembled wire entry and the OTLP record, with a built-in script covering
JSON, logfmt, klog/glog and `key=value`. Unlike `docker`, it is **not** crate-free — it is the one
feature in the workspace that adds a dependency subtree on purpose (vrl; +89 crates and +3.8 MiB on
the plugin build, §11). Three things make it safe to bolt onto an ingest hot path:

- **The event is pre-seeded with the record the driver would have stored anyway** — Docker's wire
  fields *and* the finished OTel record (body, attributes, resource, both timestamps, the stream's
  severity). The identity script `.` is therefore byte-for-byte the un-remapped behaviour, and the
  built-in script only ever *overrides*; it never re-derives `service.name`, `container.*` or
  `log.iostream`. A compiled `Program` is `Send + Sync` and lives on the `Arc<Container>`; a
  `Runtime` needs `&mut`, so one is owned per FIFO reader thread and nothing is locked.
- **Three invariants are re-asserted after every run**, because a script is operator input:
  `container.id` on the resource is *overwritten* (`ReadLogs` filters history on it, and a wrong id
  would merge two containers' histories), `service.name` is restored when absent or empty, and
  `log.iostream` is re-appended. A runtime error stores the line the un-remapped way rather than
  losing it; an explicit `abort` drops it, which is how a script filters health-check spam.
- **`docker logs` re-renders a structured body as one logfmt line** (`ts=… level=… ` then the body's
  fields) instead of the original being stored twice. A *string* body still goes out verbatim, so a
  `docker`-only build and every OTLP-ingested record are unaffected. A script may move `.timestamp`,
  but only within ±26h of Docker's capture time — `readlogs` pages and computes its follow watermark
  from it, so a skewed container clock must not be able to make `docker logs -f` skip lines.

`ReadLogs` is the one endpoint that streams: it emits length-prefixed frames for as long as the client
wants them, and the generator (`readlogs::stream`) is blocking, generic over `io::Write`, and under
`Follow` runs until the container stops. Rather than rewrite it as a `Stream`, it runs unchanged on a
`spawn_blocking` task whose sink is a bounded channel, and the response body drains that channel — so
backpressure and client disconnects arrive at the generator as ordinary `io::Error`s, and a
`docker logs -f` whose client stopped reading is abandoned after a bounded stall rather than held
open. On the wire the body is now `Transfer-Encoding: chunked`, since its length is unknowable;
Docker reads it through Go's `net/http`, which un-chunks transparently.

Unlike the library, `imbhd` **runs a flush scheduler by default**: `Maintenance::Background` plus the
`FlushPolicy` from `IMBH_FLUSH` (default `interval=5s`, i.e. seal every 5s or at the memory-budget byte
threshold), with `IMBH_MAINTENANCE_INTERVAL` (default `60s`) as the retention cadence. The library's
"no background threads unless opted in" rule is a promise to an *embedder*, and a collector process is
the host that opts in — without it, `imbhd` accumulated every row in the buffer + WAL until an operator
POSTed `/admin/flush`, so nothing reached Parquet and neither RSS nor WAL size was bounded. `IMBH_FLUSH`
takes the §10.2 spec (`interval=`, `buffer=`, `rows=`, `wal=`, `idle=`, `tick=`, or `manual`); a
malformed value is a startup error, never a silent fallback to a different cadence.

**Connections carry phase deadlines and size caps** (`Limits`). The two phases get different rules on
purpose: `IMBH_HEADER_TIMEOUT` (default `10s`) bounds the request head **in total** — hyper's
`header_read_timeout`, which also covers idle keep-alive gaps since it is armed for every head on a
connection — while `IMBH_BODY_TIMEOUT` (default `30s`) is a **per-read** allowance for the body. Neither
rule alone is right: a per-read allowance on the head lets a client dribble one byte per allowance and
hold the connection forever (never idle, never finished), while a total deadline on the body would
punish a large slow upload for its size rather than for stalling. A blown deadline is answered
`408 Request Timeout` and ingests nothing. hyper reports the head deadline but does not answer it
(`role::on_error` maps parse errors to a status and header timeouts to `None`), so the accept loop keeps
a duplicate descriptor on each socket and writes that 408 itself — only when no request head ever
arrived, so it is never appended after a keep-alive connection's last response.

`IMBH_MAX_BODY` (default `64MiB`) caps a request body, measured **after** `Content-Encoding` is undone,
so a compression bomb is refused on its inflated size rather than its wire size; an oversized
`Content-Length` is refused before a byte is read. `IMBH_MAX_CONNECTIONS` (default `512`, under the
usual 1024 soft `RLIMIT_NOFILE` so parquet and tantivy keep their share of descriptors) caps
simultaneous connections. Over the cap is `413 Payload Too Large`, and `0` disables any of the four
bounds. The body is buffered in the connection service rather than by an extractor precisely so that
"too big", "stalled", and "not actually gzip" each get their own status (413 / 408 / 400) instead of one
blanket rejection. Note the interaction with the shutdown drain below: an idle connection is cut off
before the drain gives up only when `IMBH_HEADER_TIMEOUT` is shorter than `IMBH_SHUTDOWN_TIMEOUT`, which
the stock defaults (`10s` head, `5s` drain) are not — the deadlines bound what a client can hold, and
lining the two knobs up is what makes shutdown itself prompt.

`imbhd` shuts down **gracefully** on `SIGINT`/`SIGTERM`: every accept loop stops accepting (the HTTP
one selects on a `oneshot` the token sends, then drains through hyper's `GracefulShutdown`), in-flight
requests get `IMBH_SHUTDOWN_TIMEOUT` (default `5s`) to finish, the Docker plugin's container readers
stop and its ingest queue is drained into the DB, and `Db::close()` seals the buffer before the process
exits 0. Without it the default disposition killed the process with everything since the last seal
living only in the WAL: correct (the WAL *is* the durability contract) but it meant every `docker stop`
bought a replay on the next start, and a process that owns a flush scheduler (above) should seal on the
way out. A **second** signal `_exit`s with `128 + signum` — the operator has stopped waiting.

The mechanism is a `Shutdown` token every endpoint watches (`imbh_server::shutdown`), and its two
non-obvious properties are worth keeping:

- **`accept` is woken, not polled.** A listener registers a waker on the token; both the HTTP and
  plugin loops turn that into a `oneshot` their `select!` waits on alongside `accept`, so shutdown is
  observed the moment it happens and an idle server costs nothing in between. Draining what is already
  in flight is hyper's `GracefulShutdown`, bounded by `IMBH_SHUTDOWN_TIMEOUT`. The gRPC side is the
  exception and does poll a 50 ms tick inside tonic's `serve_with_shutdown` future, which costs nothing
  per request because HTTP/2 connections are long-lived.
- **The signal handler only does async-signal-safe work**: an atomic store plus one byte down a
  self-pipe. A watcher thread parked on the read end takes the locks and notifies the condvar. Tripping
  the token from the handler would take a mutex in a signal context.

`main` waits for each endpoint to report that it has stopped *and* drained (a channel, not a `join`), so
one wedged listener cannot hold up the final seal. Signal handling needs `libc` (std cannot catch
`SIGTERM`), which is **already in the graph** via DataFusion, so the footprint is unchanged; it is
Unix-only, and elsewhere `install_signal_handlers` reports `Unsupported` and `imbhd` warns and serves on.
A host embedding the library drives the same token directly (`serve_until`, `serve_plugin_until`,
`serve_grpc_until`) with no signal handling involved.

Both `imbhd` listen addresses (`IMBH_LISTEN_ADDR`, `IMBH_GRPC_LISTEN_ADDR`) read from the environment
as well as from positional args, because a managed plugin's `entrypoint` is frozen in its
`config.json` while `env` entries declared `settable` can be changed with `docker plugin set`; an
**empty** value disables that listener, and with both off the process serves only the plugin socket
and opens no network port. `main` runs every configured endpoint on its own thread and parks on the
shutdown token, which is what makes them independently optional. *Measured, not assumed:*
`network.type: bridge` is accepted by the daemon but unimplemented for managed plugins (the process
gets an empty netns -- `lo` only, no routes), so `host` plus a bridge-gateway bind is the only way to
be container-reachable; the empty-listener setting is the supported way to be reachable by nothing.

Both addresses accept `auto`, the plugin's default, which resolves at **run time** to every bridge
gateway the daemon has and is re-resolved on a timer (`docker::serve::supervise`, one listener per
address as a task on one runtime). It replaces a single address baked in at package time by
`build.sh`, over a hard-coded `172.17.0.1` fallback -- which silently produced an unreachable
endpoint on any daemon with a custom `bip`, a re-created `docker0`, or an install that never ran
`docker plugin set`. Binding a **literal** address is fatal as it always was; binding a discovered one
is a warning and a retry. Discovery itself (`docker::networks`) has two backends tried in order on
every refresh: the Engine API over the daemon's Unix socket -- network names, IPAM gateways/subnets
and per-network container attachments -- and, when that socket is not reachable, `getifaddrs` in the
host netns, which yields the same gateways and subnets because Docker programs a bridge interface's
address from its IPAM gateway. The shipped plugin keeps `mounts: []`, so it runs in scan mode; a
standalone `imbhd` gets the API. The **hard constraint** on that half: the Engine API is never called
from `StartLogging` -- `dockerd` runs that handler while holding the container's lock and the API's
network-inspect path resolves attached containers, so a call back can deadlock the daemon against its
own log driver. Records therefore take `container.network.*` from the last published snapshot, and a
container that started between two scans picks them up when the next one swaps its resource
(`Container::set_networks`, behind an `RwLock<Arc<Resource>>` read once per batch group). The same
snapshot backs `IMBH_ALLOW_FROM`, an accept-time CIDR filter whose `docker` token expands to the
discovered subnets plus loopback -- the mitigation for `/admin/*` being unauthenticated and reachable
by every container on the box. All of it is behind the `docker` feature and adds **no crate**: `libc`
and `tokio-stream` are both already in the default graph.

Shape: one reader thread per container FIFO reassembles Docker's split lines, and all readers funnel
into a single batching worker that ingests through `Db::ingest_otlp_logs` — the same entry point the
HTTP/gRPC routes use, so there is still exactly one ingest path. Container identity becomes OTel
**resource** attributes (`container.id`/`name`/`image.*`/`runtime`, plus operator-selected labels and
env), the stream becomes `log.iostream` plus a configurable severity, and `ReadLogs` serves
`docker logs` (history, `--tail`, `--since`/`--until`, follow) back out of the store through the typed
`LogQuery` API. A full back-pressure-not-loss policy applies: a saturated ingest queue blocks the FIFO
reader rather than dropping lines. The endpoint is off unless `IMBH_DOCKER_PLUGIN_SOCKET` names a
socket, so a local `imbhd` built with the feature still never touches `/run/docker`. Packaging
(`config.json`, rootfs `Dockerfile`, `build.sh`) lives in `crates/imbh-server/docker-plugin/`; the
operator guide is `docs/DOCKER_LOG_DRIVER.md`.

> *Deviations vs the original design:* there is **no HTTP mapping of the typed §10 APIs**
> (Loki/Tempo/Prom-shaped routes) — only OTLP ingest + SQL + stats/admin; **no TOML config file**;
> and no in-process TLS (front with a proxy). OTLP/gRPC is now supported, but only under the optional
> `grpc` feature (the default build is HTTP-only). Health is `/health` (not `/healthz`). The Docker
> log-driver plugin is a post-M6 addition, not part of the original plan. Its known gaps: Docker's
> `labels-regex`/`env-regex` and `tag` log-opts are unsupported (a regex engine and a template
> language are not worth the footprint), and follow mode advances by timestamp, so two records sharing
> one nanosecond would be reported once.

### 10.17 Query-binding surface (`proto` feature)

The scaffolding for out-of-process language bindings (a Go binding is the motivating case), gated
behind the off-by-default `proto` feature. The split follows the shape of the data:

- **Inputs = protobuf.** The typed query builders (`LogQuery`/`TraceQuery`/`MetricQuery`/
  `HistogramQuery`/`ExpHistogramQuery`/`SpanMetricsQuery`) are small, nested, heterogeneous values —
  a row/tree codec, not a columnar one. `imbh-proto` holds their protobuf wire types (generated from
  `crates/imbh-proto/proto/imbh/v1/query.proto` by **protox** at build time — pure-Rust, no system
  `protoc`), and `imbh` provides `TryFrom<imbh_proto::…>` for each builder (re-exported as
  `imbh::proto`). A single `.proto` schema is the source of truth, so the Rust and binding types stay
  byte-compatible by construction. The mappings go through the builders' public setters and validate
  the wire→domain narrowing (out-of-range enum discriminant, severity > 255, negative duration,
  `usize` overflow) as user errors. The scalar operators become proto enums (`NumOp`, `LabelOp`,
  `Aggregation`, `RateMode`, `Direction`, `MetricTable`).
- **Results = Arrow, out of band.** Bulk result rows are *not* modeled in protobuf: the
  `*_batches` query entry points (`LogsApi::query_batches_with_stats`, `MetricsApi::range_batches`,
  `TracesApi::span_metrics_batches`) run the same SQL as their DTO twins but return the raw
  DataFusion `RecordBatch`es (plus a `QueryStats`), skipping row-DTO materialization. A binding then
  shares those batches **zero-copy** via the Arrow C Data Interface (or serializes them to Arrow IPC
  bytes as a fallback where a real serialization boundary is unavoidable). Zero-copy is preferred over
  ABI simplicity, so the columnar payload never round-trips through protobuf. Only the small
  `QueryStats` envelope crosses back as protobuf (`imbh::proto::encode_query_stats`). Canonical batch
  schemas: logs → the `logs` projection in schema order; metric range / span RED → `bucket` +
  `g0..gN` label columns + the value/aggregate columns. The logs entry point is the stats-returning
  twin of the **ungated** `LogsApi::query_batches` (§10.6): the two must differ in *name*, not only
  in feature gate, or they collide as duplicate inherent methods as soon as `proto` is enabled.

The FFI layer itself (the `extern "C"` / Arrow C Data Interface `cdylib`) and the Go package are a
**separate project**, out of scope for this repo; this feature is only the in-repo foundation they
consume. Histogram-quantile and trace-assembly (`search`/`get`) batch variants are a deliberate
follow-up (they involve UDF post-processing / Rust-side reshaping rather than a batch passthrough).

### 10.18 Query-language semantics, translators, and companion TUI

Language compatibility is an optional top-tier surface. Native typed builders retain their existing
IMBH semantics; they are not silently reinterpreted as PromQL, LogQL, or TraceQL.

`imbh-lgtm` is the LGTM-stack query-language compatibility crate: it targets the query surfaces of
Loki (LogQL), Tempo (TraceQL), and Prometheus/Mimir (PromQL), and is deliberately stack-specific
rather than a neutral cross-ecosystem layer (the "G", Grafana, is a dashboard UI with no query
language, so the crate implements the L/T/M languages). It splits into two modules: `model` owns
parser- and engine-independent expression models, reference evaluators, and — under the optional
`source` feature — the IMBH source adapter; `syntax` owns the parsers/translators (below).
Production execution is source-backed: `plan_prom_fetch`,
`plan_log_fetch`, and `plan_trace_fetch` derive bounded storage requests from `EvalRange` and
`EvalLimits`, then `execute_prom`, `execute_log_range`, and `execute_traceql` enforce the source
contract. A reference evaluator may receive a complete *bounded working set*; no production API
requires an entire retained series, log history, or trace corpus in memory.

The first compatibility profiles are explicit and immutable:

| Capability id | Reference version | Implemented surface |
|---|---|---|
| `imbh.promql.p1.v1` | Prometheus 3.12.0 | selectors and four matchers; instant/range boundaries and lookback; `rate`; `sum`/`avg`/`min`/`max`/`count` with `by`/`without`; cumulative classic-histogram `histogram_quantile` |
| `imbh.logql.l1.v1` | Loki 3.7.2 | explicit stream schema; four stream matchers and four exact line filters; sliding `count_over_time`/`rate`; offset; grouping; pipeline error/state contract |
| `imbh.traceql.t1.v1` | Tempo 2.10.5 | typed scoped attributes and intrinsics; spanset logic; child/parent/ancestor/descendant/sibling relations and union variants; `count()` comparison |

Within a profile, boundary and absence behaviour follows the reference implementation exactly, even
where it disagrees with the other two languages or with SQL. Three rules are load-bearing enough to
state here, because each is easy to "simplify" into a divergence:

- **PromQL lookback is left-open**, `(at - lookback, at]`. A sample exactly `lookback` old is
  *dropped*, matching Prometheus's `vectorSelectorSingle` rule (`t <= refTime - lookbackDelta`);
  instant and range selection use the same open lower bound.
- **A TraceQL condition never matches a span that lacks the referenced attribute** — negated
  operators included. `!=` / `!~` on a missing attribute evaluate to *not matched*, not true; this is
  Tempo's semantics, not SQL/PromQL three-valued NULL logic. The only presence test is the explicit
  `nil` literal: `{ .foo = nil }` matches a span missing `foo`, `{ .foo != nil }` does not.
- **A duplicated timestamp is resolved by value, never by scan order.** Under `Duplicates::LastWins`
  (§10.5.1) the surviving point of a duplicated instant is picked by a total order on the value, so
  the result is a pure function of the fetched sample multiset. "Keep whichever the scan emitted
  last" looks equivalent and is not: there is no ingest-sequence column, so it would let two
  identical queries disagree after a flush or a compaction.

The contrast with the native surface is deliberate and must not be homogenized: IMBH's own
`attr_not_in` matcher *is* NULL-aware and keeps rows lacking the key (§10.4), and PromQL label
negation keeps label-absent series. Absence semantics belong to each language, not to the engine.

The native facade is semantics-independent. It exposes bounded `MetricPointsQuery`, exact
`LogQuery` string predicates, inclusive storage ranges, and assembled-trace-start candidate bounds.
These typed models compile all user-supplied metric names, label keys/values, regexes, text, and time
bounds as DataFusion bind parameters; only fixed structural SQL is emitted internally.

Under its opt-in `source` feature, `imbh-lgtm` depends on `imbh` and owns
`build_metric_point_queries`, `build_log_query`, `build_trace_query`, plus the
`MetricsSemanticsExt`, `LogsSemanticsExt`, and `TracesSemanticsExt` execution traits. The adapter
contains no SQL. Histogram execution accepts only cumulative OTLP histograms with stable explicit
bounds; incompatible temporality or boundary changes are errors. Trace execution fetches candidate
trace ids first and then complete bounded traces, so structural operations never run over partial
traces.

`imbh-lgtm`'s `syntax` module is a dependency-light syntax adapter. `translate_promql`,
`translate_logql`, and `translate_traceql` lower only the profiles above into
`ImbhQueryModel`; unsupported valid constructs return a source-positioned stable `Diagnostic`.
`TranslateContext` resolves Prometheus metric names to exact IMBH storage names and kinds. Missing
or ambiguous catalog metadata is `NeedsResolution`, never a guess.

`imbh-tui` is an optional read-only host above the facade and the `imbh-lgtm` crate. Its binary opens a
local directory with `Db::open_read_only`; its library exposes `run(Arc<Db>, Options)`. It renders
overview statistics, PromQL metric series, TraceQL results with a client-side waterfall, and a log
viewer plus LogQL-derived count/rate charts. It coalesces refreshes to one query in flight and rejects
stale generations. Ratatui/Crossterm dependencies remain confined to this crate and do not enter the
`imbh` or `imbhd` graphs.

The exact capability and deferred-construct matrices live in
[QUERY_SEMANTICS_CONFORMANCE_PLAN.md](./QUERY_SEMANTICS_CONFORMANCE_PLAN.md),
[QUERY_LANGUAGE_TRANSLATORS_PLAN.md](./QUERY_LANGUAGE_TRANSLATORS_PLAN.md), and
[TUI_PLAN.md](./TUI_PLAN.md). Expanding a profile requires evaluator tests before parser support.

### 10.19 Head API (`imbh-head`)

A **head** is a user interface with no database of its own. `imbh-tui` is the one that ships, but
nothing in this surface is specific to a terminal. A head reads its data one of two ways, and the
head API is what makes the second possible:

```text
  imbh-tui <dir>                   imbh-tui --url http://host:4318
       │                                       │
       │ exec::*(db, req)             client::HeadClient (HTTP + JSON/Arrow IPC)
       ▼                                       ▼
     Db                              imbhd  ──►  exec::*(db, req)  ──►  Db
```

Locally the head opens the directory with `Db::open_read_only`, which takes no writer lock and so
reads *alongside* a running `imbhd`. What that view cannot see is the writer's **unsealed buffer** —
i.e. the most recent telemetry of all — which is precisely what a live UI wants. `--url` therefore
asks the daemon instead, and as a side effect the database may live on another machine.

**One implementation, two transports.** `imbh_head::exec` is the single implementation of every
operation over a `Db`. `imbh-server` calls it behind `POST`/`GET /api/head/…`; `imbh-tui`'s local
backend calls it directly, in-process. That is the load-bearing property and the reason the surface
is a crate rather than a set of routes: the query-language translation, the evaluation caps, and the
trace-window narrowing all happen in the same code either way, so the two modes cannot answer the
same question differently. `crates/imbh-server/tests/head_e2e.rs` asserts exactly that, operation by
operation, over a real loopback socket.

The eleven operations are the ones a head cannot synthesize from anything else `imbhd` serves:
`stats`, `metrics/catalog`, `metrics/promql`, `metrics/exemplars`, `traces/search`, `traces/get`,
`logs/query`, `logs/volume`, `logs/logql`, `attributes/keys`, `attributes/values`. Four of those have
no counterpart anywhere else in the server: nothing else **evaluates** PromQL, LogQL, or TraceQL
(`query_metric_range`/`search_traces` are the *typed-builder* path — they cannot express
`sum by (svc) (rate(x[5m]))`), and nothing else surfaces exemplars. `logs/query` additionally carries
the `PageCursor` and span-id correlation that the viewer's paging and trace drill-down need.

**Why not `/mcp`.** The MCP endpoint (§10.16.1) answers the same database for an *agent*: its tools
are shaped for a model (`since` windows, prose descriptions, one JSON document per call) and are
deliberately lossy — no paging cursors, no per-sample matrices, no waterfall. Reshaping them to serve
a UI would change what every agent sees. The two surfaces share the `Db` and nothing else, which is
also why the head API has a prefix of its own: `/api/head/*` is one unit a deployment can gate,
disable, or mount behind a proxy.

**Two response codecs, split by shape.** Requests are always JSON — they are query *descriptions*,
not tables. Row-shaped results (the PromQL/LogQL matrices, the TraceQL matches, a log page, a trace)
answer as **Arrow IPC**; the small scalar ones (stats, catalog, exemplars, attribute vocabularies) as
JSON. The reason is soundness, not taste: JSON has no `NaN`, no `Infinity`, and no `-Infinity`, and
`serde_json` writes all three as `null`, which then fails to read back as an `f64` — and a PromQL
evaluation produces all three routinely (`histogram_quantile` over an empty window, a division by
zero). Arrow stores the IEEE-754 bits, so the question does not arise. It is also the format these
results are already in, and `arrow-ipc` is already compiled wherever DataFusion is, so the codec
costs **no dependency**. The series schema is deliberately the one `imbh_lgtm::prom_matrix_schema`
already defines (`{labels, ts, value}`, long form), pinned by a test. Anything not row-shaped — a
paging cursor, the scan counters, a trace's assembled header, the narrowed window start — rides in
the IPC schema's custom metadata, so a response stays one self-describing message. Failures are JSON
in both cases, in the same `{"error": …}` shape as the rest of `imbhd` plus a `kind` discriminator a
head branches on (the trace search retries on `limit_exceeded` and gives up on anything else).

The encoders take the **materialized** result types rather than the engine's `*_batches` twins. That
is what keeps `exec` at one return type for both backends — a local head uses the value and encodes
nothing — and it keeps `imbh/proto` (and its protox codegen) out of both binaries. The server pays
one extra materialize-then-encode on a page of at most a few thousand rows, which is not a cost worth
a second execution path.

**An eval request carries every sub-query**, because a head routinely asks for several: the metric
catalog emits one selector per checked metric, and the evaluator has no `or`, so each must run on its
own. Sending them together is one round trip and, more to the point, **one** metric-catalog read —
the catalog is what PromQL translation resolves a selector's kind against, so a query apiece would
re-read it apiece.

**Footprint.** `imbh-head` is downstream of the facade, so the crate-count gate (`cargo tree -p imbh`)
is unchanged at 275. The feature split keeps each consumer to the half it plays: `imbh-server` takes
`exec` only, so the **client's reqwest subtree never enters `imbhd`** (297 crates, from 293; the
release binary 32.6 → 32.9 MiB against a 42 MB target). `imbh-tui` takes both and pays for the
transport (323 crates, from 313) — a binary that is already outside every library budget, and the
alternative was a hand-written HTTP client in a terminal program.

Read-only throughout: nothing below `/api/head` ingests, flushes, compacts, or applies retention.
Like the rest of `imbhd` it is unauthenticated, so a real deployment gates the prefix.

## 11. Footprint engineering (continuous, not a phase)

- **Feature matrix.** `search` (§10.13), `tracing` (self-observability), `serde` (DTO derives,
  §10.13), and `proto` (query-binding surface, §10.17) exist today, all off by default except
  `search`; per-signal / `sql` gates are planned levers, not built.
- **Profiles.** Shipped binaries use `opt-level = "s"`, `lto = "fat"`, `codegen-units = 1`,
  `strip = "symbols"`, `panic = "abort"`. Library crates never force `panic` settings on hosts.
- **Dependency policy.** No openssl anywhere; C code allowed only for libzstd (shared by parquet
  and tantivy, linked once) — **and, under `imbh-server`'s off-by-default `docker-remap` feature
  only, `onig_sys`** (vendored oniguruma), which vrl's `stdlib-base` forces via its `datadog`
  feature. That is the graph's second C library and the one exception to the libzstd rule; both
  Linux release legs build natively, so no cross-compilation setup changes, and the macOS/Windows
  legs never enable the feature. *Correction:* lz4 is Parquet's built-in `LZ4_RAW`, not the pure-Rust
  `lz4_flex` — which **is** now a dependency, but only via vrl under `docker-remap`, and not for
  compression (this sentence previously read "which is not a dependency"; see the 2026-08-06 JOURNAL
  entry). **Self-observability uses the `tracing` facade,
  feature-gated (`tracing`, off by default), never `log`** (superseding the earlier `log`-not-`tracing`
  rule — see the 2026-07-19 JOURNAL entry): library crates emit spans/events through an optional
  `tracing` dependency that compiles away entirely when the feature is off, and the `tracing`
  facade itself adds **zero** crates on top of the default graph (DataFusion already pulls
  `tracing`/`tracing-core`). The heavier `tracing-subscriber` renderer is opt-in only, with two
  owners: the `imbh` facade's stderr console collector (its off-by-default `tracing-console` feature,
  the `imbh::console` module) and the `imbh-tracing` helper crate's `DbLayer`. `imbh-server`/`imbhd`
  pull it via `imbh/tracing-console` under their own `tracing` feature (§12 containment — keep it out
  of the default library graph); measured cost is +5 crates on `imbhd` (281, well under the 300 hard limit) with
  the default facade build unchanged at 275. serde is present transitively but the default facade
  graph stays serde-free; the optional `serde` feature (§10.13) turns on DTO derives without adding a
  crate (serde is already compiled). `cargo-deny` gates licenses + duplicate-version creep (`deny.toml`); a `cargo tree`
  count budget is enforced in the footprint gate. **`deny.toml` runs `all-features = true`**, so an
  optional feature's subtree is license-checked whether or not it is on — `MIT-0` and `0BSD` are on
  the allow-list because vrl reaches them, and default-off would not have avoided that.
- **Measurement rig.** `scripts/footprint-gate.sh` checks binary size and crate count against the
  §2 budgets (see [QUALITY_GATE.md](./QUALITY_GATE.md)); `cargo bloat` / RSS soak as needed. Note
  what the gate can and cannot see: it counts `cargo tree -p imbh` (the **library** graph) and builds
  `imbh-server` with **default** features, so nothing behind an off-by-default `imbh-server` feature
  moves either number. The gate therefore also prints an **informational, never-failing** measurement
  of the shipped plugin build (`docker,docker-remap,grpc,tracing`), which is the only place
  `docker-remap`'s +89 crates and +3.8 MiB are visible.
- **Allocator.** System allocator by default.

## 12. Workspace layout

```
imbh/
  Cargo.toml               # workspace, shared lints, release-small profile
  crates/
    imbh-core/             # schemas, ids, config, errors, manifest types, canonical JSON,
                           # dependency-free JSON parser, time utils (arrow-free)
    imbh-otlp/             # OTLP decode → normalized rows (prost types), all 3 signals
    imbh-storage/          # WAL, mutable buffer, seal, segments, manifest IO, retention,
                           # compaction; owns the Arrow schemas
    imbh-index/            # tantivy schema/build/search + row-ordinal bridge (only Tantivy crate)
    imbh-query/            # DataFusion: providers, UDFs, session config (only DataFusion crate)
    imbh-lgtm/             # LGTM-stack (Loki/Tempo/Mimir) query languages: expression models +
                           # reference evaluators (`model`), P1/L1/T1 parsers/lowering (`syntax`),
                           # and the optional native-IMBH `source` adapter (feature-gated)
    imbh-mcp/              # the MCP server: protocol dispatch, the read-only tool surface, and the
                           # stdio transport (shared by imbh-server's HTTP endpoint and imbh-tui)
    imbh-tui/              # optional read-only local companion TUI; its binary also hosts the
                           # MCP stdio transport (`imbh-tui --mcp-stdio`)
    imbh/                  # facade crate embedders use: Db, blocking + async API; optional stderr
                           # console renderer (`imbh::console`, `tracing-console` feature)
    imbh-proto/            # protobuf wire types for the query inputs (protox build.rs); the
                           # `proto` binding surface (§10.17) (optional, prost-only)
    imbh-otel-exporter/    # opentelemetry-rust SDK exporter adapters (optional)
    imbh-server/           # reference imbhd binary + example HTTP wiring (optional)
    imbh-tracing/          # in-process `tracing` plumbing: `DbLayer` sinking tracing spans/events
                           # into a `Db` (self-observation, depends on imbh) (optional)
  examples/                # replay-otlp-file, embed-in-app, …
  docs/                    # EMBEDDING.md, PROMQL_TO_SQL.md
  crates/imbh-head/        # the head API: wire types, Db-side execution, HTTP client (§10.19)
  .agents/docs/            # OVERVIEW.md, ARCHITECTURE.md (this file), JOURNAL.md, TODO.md, …
```

Dependency direction: `core ← {otlp, storage, index, query} ← imbh ← {exporter, server}`. The
LGTM query languages live in `imbh-lgtm`, whose `syntax` (parsers) depends on its own `model`
(expression types + evaluators); the optional `imbh-lgtm/source` adapter depends on `imbh`, and
`tui` depends on `imbh` and `imbh-lgtm` with that feature. `imbh` never depends on `imbh-lgtm` or
terminal crates.
`imbh-mcp` sits in that same top tier — `imbh ← imbh-mcp ← {imbh-server, imbh-tui}` — and is where
the MCP protocol lives *because* of this direction: the HTTP endpoint is in `imbh-server` and the
stdio one in the `imbh-tui` binary, and neither of those crates may depend on the other (§10.16.1).
`imbh-head` sits there for the same reason — `imbh ← imbh-head ← {imbh-server, imbh-tui}` — and is a
separate crate from `imbh-mcp` because it is a separate facility: one surface is for a UI, the other
for an agent (§10.19). Its features split the two halves so neither consumer carries the other's:
`exec` (execute against a `Db`) for the daemon, `exec` + `client` (the HTTP client) for the head.
`imbh-proto` is a prost-only leaf that `imbh` depends on **optionally**, under its `proto` feature
(§10.17); it depends on nothing else in the workspace.
`imbh-tracing` sits at the top tier alongside `{exporter, server}` and depends on `imbh`. It provides
`DbLayer`, a `tracing_subscriber::Layer` that sinks `tracing` spans/events into an embedded `Db`
in-process (events → `logs`, span closes → `spans`) over the same `Db::try_ingest_otlp_*` path
`imbh-otel-exporter` uses. It is *not* internally feature-gated — the crate is itself the opt-in
boundary (a host depends on it only to wire tracing to IMBH). Crucially it depends on `imbh` with
default features, so it does **not** enable `imbh/tracing`: the sink (collect) and IMBH's
self-instrumentation (emit) stay independent opt-ins. The companion stderr *console collector* (a
`fmt` subscriber that renders IMBH's instrumentation to the terminal — `imbh::console`, the
`env_filter`/`directives`/`IMBH_TARGETS` helpers) lives in the `imbh` facade instead, behind its
off-by-default `tracing-console` feature, so hosts that only want console output never pull the
`imbh-tracing` crate. The two compose on one `tracing_subscriber` registry.
`imbh-index` is the only crate that knows Tantivy; `imbh-query` the only one that knows
DataFusion — engine churn (DataFusion ships breaking majors ~monthly) is absorbed behind these two
crates. **Engine-boundary note:** `imbh-core` is arrow-free; `imbh-storage` owns the Arrow schema
and hands the `SchemaRef` + buffer `RecordBatch` to `imbh-query` *through the facade*, so
`imbh-query` stays the sole DataFusion-aware crate without either sibling depending on the other.

## 14. Risks & mitigations

| Risk | Status |
|---|---|
| DataFusion size budget (204/269 crates) | **Owned.** ~30 MB is the price of the query engine; levers are per-signal/`search`/`sql` gates (mostly still to build). |
| Tantivy↔Parquet row misalignment (silent wrong results) | **Mitigated.** Row ordinal stored as data; index rebuilt (not merged) on compaction; shared tokenizer + differential tests. |
| Delta→cumulative accumulator | **Open / not built.** Delta series stored as-is; querying delta as cumulative is a known gap (§6.4). |
| Manifest write amplification at high segment counts | **Mitigated (built).** The manifest is an append-only delta log with a compacted checkpoint (`CURRENT` → `MANIFEST-<N>`), so a seal appends O(change) not O(total segments); the log is rolled into a fresh checkpoint past a size threshold (§7). |
| No single-writer lockfile | **Built.** A read-write open holds an exclusive advisory lock on `writer.lock`; a second writer fails fast with `Error::lock_held` (§5/§7). Enables cross-process read-only readers (§5). |
| Reader reads a segment retention/compaction just deleted | **Mitigated (Phase 3).** The read-only query path detects a segment path it snapshotted then vanished and re-derives from the current manifest, retrying up to `READER_QUERY_TRIES`; a genuine error (all paths present) is surfaced as-is. Optional writer-side deletion grace period remains a later add. See §7.1. |
| Query racing a seal double-counts or drops rows (intra-process) | **Mitigated (Phase 3).** `Storage::query_snapshot` captures buffer∪segments under one lock (no double-count) and unions the in-flight seal's `Inner::sealing` staging (no mid-seal drop during the off-lock write). §5/§7. |
| `matches()` result depends on flush timing | **Mitigated.** `matches` = tokenized-term containment; index and row-wise fallback share one tokenizer. |
| WAL replay double-counts sealed data | **Mitigated.** Per-generation LSN watermark; replay re-ingests only LSN > watermark (§7). |
| Attribute analytics on non-promoted keys | **Partial.** `json_get_str` scans work; configurable key promotion to typed columns now lands via `DbBuilder::promote` (§6.1), but no Tantivy attrs index and no automatic promoted-column pushdown in the typed/LGTM query builders yet. |
| DataFusion monthly breaking majors | **Contained.** Isolated in `imbh-query`; pinned; deliberate upgrade cadence. |

Where the implementation is **ahead** of the original plan: the cost-gated `RowSelection` bridge,
partition compaction with index rebuild, exp-histograms, exemplars, the `imbh-otel-exporter`
crate, and the opt-in background maintenance thread with a `close()` join are all built.

## 15. Open questions for review

1. **Platforms.** Plan assumes Linux x86_64/aarch64 + macOS, musl static for the reference server;
   Windows near v1?
2. **Scale envelope.** Defaults tuned for ≤ ~20 GB/day ingest, ≤ 30 d retention, single node.
3. **Name & license.** Keep `imbh` as the crate prefix? Apache-2.0?
4. **PromQL.** Post-v1, or is a minimal subset a v1 requirement? (A PromQL→SQL recipe exists in
   `docs/PROMQL_TO_SQL.md`.)
5. **Wire-shape fidelity.** Are DTOs "deliberately near" the Prometheus/Tempo/Loki JSON shapes
   enough, or should they aim for byte-compatibility so existing Grafana datasources work with zero
   translation once HTTP is wired?

Resolved by review: the deliverable is the library and server wiring belongs to the host (`imbhd`
is reference only); the API surface follows the Loki/Tempo/Mimir/SigNoz/VictoriaMetrics designs.

## Appendix A — Pinned toolchain & versions

| Component | Version | Note |
|---|---|---|
| Rust | 1.96 (edition 2024) | MSRV policy: track DataFusion's MSRV |
| datafusion | 54.0.0 | `default-features = false` + the list in §9.1 |
| tantivy | 0.26.1 | `default-features = false` + `mmap`, `lz4-compression` |
| opentelemetry-proto | 0.32.0 | prost messages only, per-signal features |
| opentelemetry / opentelemetry_sdk | 0.32.0 | used only by `imbh-otel-exporter` |
| prost | 0.14 | |
| tokio | 1.x (`rt`, `macros`) | |
| xxhash-rust | 0.8 (`xxh3`) | WAL/manifest checksums |
| arrow / parquet | 58.3.0 via `datafusion::` re-exports | never a direct dependency |

## Appendix C — M0 footprint measurements & probe

**Status:** M0 gate complete (2026-07-18). **Decision: GO** on architecture and engine choice;
budgets revised to measured reality (OVERVIEW.md §2); "SQLite-tiny" framing tempered to "compact".

A probe crate that **links and exercises** the full mandated stack (a real DataFusion SQL
aggregation over a Parquet round-trip, a Tantivy mmap index build+search, an OTLP protobuf
round-trip) — so dead-code elimination can't flatter the result — was built at the shipping
profile and measured.

| Metric (aarch64-glibc, shipping profile) | Measured |
|---|---|
| Binary size (stripped) | **33,474,928 bytes = 31.9 MiB** |
| Anonymous RSS while exercised (`VmRSS`) | 36.0 MB |
| Peak RSS (`VmHWM`) | 45.6 MB |
| Unique crates (normal edges) | **269** (datafusion 204, tantivy 110 overlapping, otel-proto 36, sqlparser 13, prost 10, tokio 8) |

**Caveats — this is a floor, not the final server number.** Measured on aarch64-glibc, not the
server's x86_64-musl (musl bundles libc; different codegen). The probe omits hyper/serde_json/toml
and all business logic, so real `imbhd` is larger. The 36 MB RSS is "exercised" (a 50 MB Tantivy
writer heap + transient Arrow batches in play), an active-use upper bound at tiny data, not the
clean idle figure — a dedicated idle harness is an M1 task. What the probe proves: sub-12 MB idle
(the original guess) is unreachable once DataFusion's `SessionContext` and a Tantivy writer exist.

**Decision detail.** DataFusion is 204/269 crates and the bulk of the binary with no cheap lever
(dropping `sql` saves ~13 crates); `opt-level="z"` yields single digits, not a halving. The honest
move is to own ~30 MB as the cost of the query engine and ship it. Real levers for the constrained
embedder (documented, not default): `search` off (drop Tantivy's subtree; `matches()` degrades to
scan), and the planned per-signal / `sql` gates. The revised budgets these produced are in
OVERVIEW.md §2.

### C.1 — Per-target shipped sizes, v0.5.0 (first cross-platform measurement)

**Provenance.** Release workflow run `31004270880`
(<https://github.com/moriyoshi/imbh/actions/runs/31004270880>), tag **`v0.5.0`**, commit `5ae4259`,
run started 2026-08-05T12:07:28Z, Release published 2026-08-05T12:46:37Z. Harvested 2026-08-06.
These are the first footprint numbers taken on the *published* targets rather than on a CI or
developer host — every figure elsewhere in this appendix, and everything
`scripts/footprint-gate.sh` prints, is a single-host reading.

**How they were obtained.** `release.yml`'s `Package` step writes exactly this table into
`$GITHUB_STEP_SUMMARY`, but **a step summary is not reachable through the REST API** — there is no
`.../actions/jobs/<id>/summary` endpoint (404), and the job's check-run `output` returns
`{"summary": null, "text": null}`. The numbers below were therefore measured from the run's
`dist-*` artifacts instead: each archive was downloaded, unpacked, and the extracted files sized.
The artifacts' SHA-256 sums are identical to the `SHA256SUMS` asset on the published Release, so
these are the bytes users download, not a reconstruction of them.

| Target (v0.5.0) | `imbhd` bytes | MiB | `imbh-tui` bytes | MiB | archive bytes | MiB |
|---|---:|---:|---:|---:|---:|---:|
| `x86_64-unknown-linux-gnu` | **41,112,104** | **39.2** | 40,497,648 | 38.6 | 27,942,219 | 26.6 |
| `aarch64-unknown-linux-gnu` | **35,973,464** | **34.3** | 35,354,648 | 33.7 | 26,164,419 | 25.0 |
| `x86_64-apple-darwin` | **36,963,516** | **35.3** | 36,637,084 | 34.9 | 26,817,413 | 25.6 |
| `aarch64-apple-darwin` | **30,898,672** | **29.5** | 30,565,984 | 29.1 | 23,681,138 | 22.6 |
| `x86_64-pc-windows-msvc` | **35,489,792** | **33.8** | 35,088,896 | 33.5 | 24,673,322 | 23.5 |

The two binary columns are uncompressed bytes at the shipping profile (`opt-level="s"`, fat LTO,
`codegen-units=1`, `strip="symbols"`, `panic="abort"`). The archive column is the **compressed**
`.tar.gz` (`.zip` on Windows) and additionally carries `LICENSE` plus a 484,064-byte
`THIRD-PARTY-NOTICES.txt`; it is not comparable to a binary size and must not be quoted as one.

**What build these are.** Not the default-feature build the §2 budgets describe. At `v0.5.0` the
Linux legs were `--features docker,grpc,tracing` and the macOS/Windows legs `--features
grpc,tracing` (both omit `docker`: a log-driver plugin is served over a Unix socket to a *local*
daemon). `docker-remap` — the VRL subtree, +89 crates / +3.8 MiB when measured on aarch64-glibc —
was added to the Linux legs *after* this tag, so **no released archive has been measured with it
yet**; that number stays unavailable until the next release run.

**Readings.**

- **The spread across targets is ~1.33×**, from 30.9 MB (`aarch64-apple-darwin`) to 41.1 MB
  (`x86_64-unknown-linux-gnu`). x86_64 is the larger of each pair; on Linux, identical source at an
  identical profile is 5,138,640 bytes heavier on x86_64 than on aarch64.
- **`imbh-tui` is only 0.3–0.6 MB smaller than `imbhd` on every target.** The explorer is not a
  cheap add-on — both binaries link essentially the whole DataFusion+Tantivy stack — so shipping the
  pair roughly doubles each archive's uncompressed payload.
- **Against §2's `imbhd` budget** (≤ 42 MB target, ≤ 55 MB hard, compared as decimal MB by
  `scripts/footprint-gate.sh`), every target is inside the target, but the largest —
  `x86_64-unknown-linux-gnu` at 41.1 MB — has just **887,896 bytes of headroom**. The 5-plus-MB
  x86_64/aarch64 gap means the aarch64 host figure the gate normally reports is the *optimistic*
  end of the range, not a representative one.
- The `aarch64-unknown-linux-gnu` figure is byte-identical to the pre-`docker-remap` baseline
  recorded in the 2026-08-06 JOURNAL entry, confirming that entry's host measurement and this
  archive are the same build configuration.

### C.2 — Recommendation (not a decision): the musl question

**What is built vs. archived, from the same run.** `release.yml`'s matrix builds **five** targets
and archives **all five** — there is no target that is built but not shipped, and the five Release
assets match the five matrix legs one-for-one. The two `*-unknown-linux-gnu` archives are then
reused downstream: unpacked into the multi-arch `ghcr.io/moriyoshi/imbh` image and into the two
per-arch `imbh-log-driver` plugins. `x86_64-unknown-linux-musl` is **not built anywhere in CD**. It
appears only in `about.toml` and `deny.toml`'s target lists, where it makes the license/notices gate
cover musl-specific dependencies, and as the target the plugin rootfs used before that Dockerfile
switched to copying a prebuilt glibc binary.

**Recommendation: do not add a musl release archive. Retarget §2's `imbhd` budget to glibc
x86_64 instead.** Four reasons:

1. **The budget names a target nothing ships.** §2 picked musl when it was expected to be the
   shipping target; it is not. Every installable artifact — five archives, the image, both plugins —
   is glibc, MSVC, or Mach-O. A budget defined on a target that is never built can never be checked,
   which is the exact gap C.1 was harvested to close.
2. **A sixth leg is not free, and it reintroduces the problem the matrix was designed around.** Each
   leg is a fat-LTO, `codegen-units=1` build (~40 minutes wall-clock for five in parallel). There is
   no native musl GitHub runner, and `zstd-sys` — plus `onig_sys` under `docker-remap` — builds
   vendored C, so a musl leg needs `cross`/`zigbuild` or a container: the one configuration
   `release.yml`'s matrix comment says was deliberately avoided by building Linux natively.
3. **The demand is already covered.** The usual reason to want musl is Alpine or `FROM scratch`.
   imbh's published image is `debian:bookworm-slim`, and the release binaries are pinned to a glibc
   2.35 floor (built on `ubuntu-22.04`) and CI-asserted at ≤ 2.36 so they run on it. An Alpine user
   today is one `docker plugin install` or one `ghcr.io/moriyoshi/imbh` pull from a working
   deployment.
4. **If the musl *number* is what is wanted, measure it without shipping it.** A `workflow_dispatch`
   rehearsal leg, or a single local `cross build`, yields the comparison figure at a fraction of the
   standing cost — and does not create a sixth asset that must be checksummed, supported, and kept
   working forever.

The follow-up this implies is a documentation change, not a pipeline change: restate §2's `imbhd`
row as a glibc x86_64 budget with the measured 41.1 MB beside it, and keep musl as a noted,
unmeasured variant. **That edit is deliberately not made here** — changing a budget number is the
user's call, not this measurement's.
