# Storage Engine: WAL, Durability, Manifest, Compaction, and Retention

## Summary

The imbh storage engine (`imbh-storage`) provides durable, crash-recoverable ingest built on a write-ahead log (WAL), idempotent watermark-gated replay, an append-only manifest delta log with compacted checkpoints, immutable Parquet segments, in-place compaction, and retention-based deletion. It is the only durability boundary in the suite: every crash point reduces to "replay the un-reclaimed WAL tail", and the single invariant that keeps this correct is durability ordering — make the metadata edit durable *before* reclaiming WAL space or deleting files. Optional async ingest and an opt-in background maintenance scheduler move work off the caller while preserving the "no background threads unless opted in" guarantee.

## Key Facts

- **WAL frame format**: append-only frames `(len, xxh3, lsn, signal, payload)` where `payload` is the raw OTLP export-request bytes and `xxh3` is XXH3-64 (pure-Rust `xxhash-rust`, per §11 no-C policy) covering `len || lsn || signal || payload`. `read_frames` stops at the first torn/checksum-failing frame and enforces strictly-increasing LSNs.
- **Idempotent replay via a watermark (§7)**: the manifest stores `#watermark <lsn>` = the highest LSN captured in sealed segments. On open, WAL records with `lsn > watermark` are replayed (buffer-only); records `<= watermark` are never replayed → no double-count.
- **WAL modes**: `Db::builder(..).wal(WalMode)` with `Off` / `Interval(d)` / `Always` (default `Interval(1s)`). `ingest_otlp_logs` fsyncs under `Always` (durable receipt); `try_ingest_otlp_logs` never fsyncs inline (§10.5).
- **Durable write pattern (fsync everywhere)**: `write_parquet` `sync_all`s the finished file; `write_segment_parquet` fsyncs the day-partition dir after rename; `persist_manifest` writes temp → fsync → rename → fsync-dir. Seal ordering: segment-durable → manifest-durable → WAL-truncate.
- **Manifest v2**: append-only delta log (`CURRENT` → `MANIFEST-<NNNNNN>`) with a checkpoint roll past `CHECKPOINT_BYTES` (256 KiB), replacing the M0 whole-file rewrite and its O(total segments) write amplification.
- **Compaction** (`db.compact()`): merges same-UTC-day sealed segments per table; rebuilds the logs `.tidx` from the merged, time-sorted batch.
- **Retention** (`Storage::retain` / `Db::maintain`): the only deletion path; drops age-expired and/or over-budget segments.
- **Async ingest** (`Ingest::Async`) and **group-commit fsync** move WAL append + encode + buffer push off the caller and batch fsyncs per drained burst.

## Details

### WAL and idempotent replay (M1a, §7)

The WAL (`imbh-storage::wal`) is an append-only log of frames `(len, xxh3, lsn, signal, payload)`. `payload` is the raw OTLP export-request bytes; `xxh3` is XXH3-64 over `len || lsn || signal || payload`. `read_frames` stops at the first torn or checksum-failing frame — the expected shape of a crash mid-append — and returns everything before it, and enforces strictly-increasing LSNs (it stops at a non-monotonic but checksum-passing region).

Recovery is idempotent via a watermark. The manifest stores `#watermark <lsn>` = the highest LSN captured in sealed segments. On seal, the watermark is bumped to the highest *buffered* LSN (`buffer_max_lsn`, tracked under the same lock as ingest so there is no assign-vs-append race). On open, WAL records with `lsn > watermark` are staged (`take_pending_replay`) and the facade decodes and re-ingests them (`Storage::replay`, buffer-only); records `<= watermark` are never replayed.

The seal watermark must be the highest LSN whose rows are *actually in the buffer* (`buffer_max_lsn`), not the highest LSN handed out — otherwise a concurrent ingest that got an LSN but has not appended its rows yet would be marked "sealed" → silent data loss on replay. Assigning the LSN and appending rows under one `Inner` lock (with the WAL append inside that critical section) closes the window. `AtomicU64::fetch_max` advances `durable_lsn` from both paths (`Always` fsync and seal) without racing.

**Engine boundary preserved**: replay is orchestrated by the facade (the only crate that may depend on both `imbh-storage` and `imbh-otlp`). Storage exposes raw WAL records; the facade decodes them via `imbh-otlp` and calls back into `storage.replay`. Storage never learns OTLP.

**API surface**: `Db::builder(..).wal(WalMode)` (`Off` / `Interval(d)` / `Always`, default `Interval(1s)`); `IngestReceipt.durable` / `lsn`; `Db::durable_through()`. `ingest_otlp_logs` fsyncs under `Always`; `try_ingest_otlp_logs` never fsyncs inline (§10.5). Seal writes Parquet to a temp path then renames (§7 atomicity); `durable_lsn` advances on both `Always` fsync and seal.

`WalMode::Interval` fsync is approximate: fsync happens on `flush` / `close` / `Always`, not on a background timer (the §5 "no background threads unless opted in" guarantee).

### WAL truncation after seal (§7)

`Wal::truncate_below(path, watermark)` rewrites the WAL keeping only frames with `lsn > watermark` (temp file → `sync_data` → rename → reopen the append handle). Frame serialization is shared via `encode_frame` (used by both `append` and the rewrite). `seal()` calls it after the manifest is durable, under the WAL mutex so no append races.

Ordering makes it safe: write segments (durable) → persist manifest with the new watermark (durable) → truncate WAL. The manifest watermark is the source of truth; replay always skips `lsn <= watermark`, so a crash before or after truncation only changes reclaimed space, never correctness — truncation is a pure optimization. Concurrency: `seal` drops the `inner` lock before truncating and holds the WAL mutex for the whole rewrite, so a concurrent ingest either appended its `lsn > watermark` frame before the rewrite (preserved by the filter) or blocks on the WAL mutex and appends to the reopened handle afterward. Because the live `Wal` holds an append `File` on the old inode, the handle is reopened on the compacted path inside `truncate_below`.

### Orphan-segment cleanup on open (§7)

`cleanup_orphans(dir, &manifest)` is called from `Storage::open` right after `load_manifest`. It walks the DB tree and deletes any `*.parquet` file (and its `*.tidx` sidecar dir) not referenced by the manifest, plus stray `*.tmp` / `*.compact` temps from interrupted segment/manifest/WAL writes (and `MANIFEST-*` / `CURRENT.tmp` from an interrupted manifest roll). Best-effort — per-file unlink errors are swallowed; it never fails open.

The manifest is the source of truth and is persisted last (temp → rename) on every seal / retain / compact, so anything it does not name is provably dead: either a seal crashed after writing segments but before persisting the manifest (its WAL frames are still `> watermark` and replay re-derives them), or a compaction/retention crashed mid-swap. Deleting non-referenced files can never lose durable data. The walk skips descending into `*.tidx` sidecar dirs (opaque Tantivy indexes) and leaves `wal.log` / `MANIFEST` alone. Together with WAL truncation, a crash-and-reopen converges to exactly the manifest's state plus the replayable WAL tail — no unbounded accumulation of dead segments or WAL frames.

### Append-only manifest delta log + compacted checkpoint (§7 Tier-C)

The M0 whole-file manifest was rewritten in full on every seal/retain/compact (O(total segments) write amplification). It is replaced by an append-only delta log + compacted checkpoint in the self-contained `crates/imbh-storage/src/manifest.rs` module. The `Manifest` type moved from `lib.rs` into the module.

**On-disk format (v2)**:
- `CURRENT` — a tiny text file naming the active `MANIFEST-<NNNNNN>`, replaced atomically (temp → fsync → rename → fsync-dir).
- `MANIFEST-<NNNNNN>` — an append-only log of framed records `| len(4) | xxh3(8) | payload |`. The first frame is a **checkpoint** (a `reset` edit with the full segment set + watermark); later frames are **deltas** (line-based `W` / `+` / `-` records). Replay applies frames in order and stops at the first torn/checksum-failing frame (torn-tail-tolerant, exactly like the WAL).

**Writer**: `ManifestWriter` holds `{current_num, log_bytes, last: Manifest}`. `persist(view)` diffs the new in-RAM state against `last` (path-identity set-diff per table; segments are immutable and uniquely named, so path identity is stable) and either no-ops (empty diff), appends the delta frame (fsync), or — past `CHECKPOINT_BYTES` (256 KiB, ~5k segment edits), or on the first persist of a fresh DB — writes a fresh checkpoint and flips `CURRENT` (a **roll**), then unlinks the old log. Callers keep the full-state contract (Approach B: diff internally), so the three persist call sites changed only from a free `persist_manifest(&dir, …)` to `self.persist_manifest(…)` reading `self.manifest: Option<Mutex<…>>`.

**Crash safety**, all reducing to the existing model:
- *Torn delta append* → dropped by the frame scan → the seal "didn't happen" durably → its rows replay from the still-unreclaimed WAL (reclaim runs only after persist returns).
- *Crash mid-roll* → the new `MANIFEST-<N+1>` is durable before `CURRENT` flips and the old log is unlinked only after, so `CURRENT` always names a complete log; a crash on either side leaves a stray `MANIFEST-*` that `cleanup_orphans` sweeps.
- *Reader during a roll* → readers resolve `CURRENT` atomically and replay one log; if a roll unlinked it mid-read, `manifest::read` re-resolves `CURRENT` (bounded) — the new checkpoint holds everything the old log did, so no drop. This is why WAL-reclaim-after-durable-edit stays correct: a reader can never end up on a stale checkpoint missing already-reclaimed rows.

Durability ordering is unchanged (edit fsync'd before WAL reclaim / segment deletes). The watermark-only re-check bracket in `read_disk_snapshot_incremental` remains sufficient — `read` returns an internally-consistent point-in-time manifest, and file-deletion races remain handled by the separate `collect_with_stats` read-during-delete retry.

**Compat & migration**: a legacy M0 whole-file `MANIFEST` is read and migrated to v2 on the first writer open (v2 written durably before the legacy file is unlinked — no lossy window); a read-only reader reads legacy as-is without migrating. `snapshot()` writes the destination a fresh self-contained checkpoint (`CURRENT` + `MANIFEST-000001`) from the in-RAM state instead of copying the old file, so a snapshot dir opens like any other DB.

Design choices: diff-internally (Approach B) over hand-computed deltas (mechanical set-diff is correct by construction); roll writes a full checkpoint rather than a compacted-delta rebase (simpler, self-contained); no open manifest file handle (each append reopens append + fsyncs — one extra open syscall per persist, negligible, avoids storing a `File` in `Inner`/`Storage` and its lifetime/poisoning concerns).

### Compaction (M4b)

`compact()` — for each table, groups sealed segments by UTC-day partition (parsed from the `<table>/<day>/…` path) and merges each group of `>1` segments: read all their Parquet → `concat_batches` → `sort_to_indices` / `take` by the table's time column → write one merged Parquet via `write_segment_parquet` → delete the inputs → rewrite the manifest. Returns `CompactionReport { segments_merged, segments_created }`. The generic path handles all tables via `compact_partition(table, time_col, build_index)`.

**Logs index rebuild**: after merging a logs partition it rebuilds the `.tidx` from the merged, time-sorted batch. To keep `imbh-index` arrow-free, storage converts the batch back to the minimal `LogRow`s the index needs (`body` / `service` / `severity_text`, via `logs_batch_to_index_rows`) and reuses `build_logs_index`. The row ordinal stays aligned because the index is built from the exact batch that was written — rebuild from the sorted batch, not the source rows, so the index `row` ordinal matches the merged Parquet's row order (the §8 "alignment is data, not assumption" invariant survives compaction). Spans/metrics compact with no index (they have none). In-memory merge (read the whole partition into batches) is fine at embedded scale; the plan's streaming merge-sort is a follow-up for very large partitions.

**Coverage of List-column metric tables**: `compact()` iterates all keys present in `metric_segments` (collected first to avoid mutating while iterating), not just `SCALAR_METRIC_TABLES` (gauge, sum). This ensures `metrics_histogram` / `metrics_exp_histogram` / `metrics_summary` segments are compacted; `compact_partition` is schema-agnostic (concat + sort by `time`, no Tantivy index for metrics) so it merges the List columns correctly.

### Retention (M1e, §3/§7)

Retention is the only deletion path — data is otherwise immutable. `imbh-core::Retention` — `days(n)` and/or `max_disk_bytes(b)`; both optional, combined with OR (age-expired **or** over the disk cap). `Storage::retain` drops age-expired segments (`max_time < now - max_age`), then, if a disk budget is set, drops oldest-first (by `min_time`) until total on-disk size (Parquet + `.tidx`) is under budget. It deletes both the `.parquet` and the `.tidx` dir and rewrites the manifest. Returns `RetentionReport { segments_dropped, bytes_freed }`.

`Db::maintain` = seal + retain → `MaintenanceReport { sealed, segments_dropped, bytes_freed }` — the inline path behind §5's "no background threads unless opted in"; `DbBuilder::retention` configures the policy.

The watermark is untouched by retention: dropped segments' WAL records are `<= watermark`, so they never replay — no risk of resurrecting deleted data. Dropping is per-segment (by time/size), not per-day, which is equivalent at segment granularity. Segment on-disk size must include the `.tidx` sidecar dir (recursive `dir_size`), not just the Parquet file, or the disk-budget accounting under-counts and keeps too much. File deletion must tolerate already-absent paths (`NotFound` → ok).

### Storage durability review — fixes (HIGH/MEDIUM/LOW)

An adversarial review verified the correct parts (WAL stop-on-corruption, watermark-gated replay, `cleanup_orphans` as source-of-truth, compaction row-preservation, lock ordering) and found real durability defects: the engine had no fsync anywhere except the WAL, and three operations deleted/truncated state before the replacement was durable. All addressed across three iterations:

- **HIGH 1 — seal truncated the WAL after never-fsync'd segment+manifest writes** (power loss → data loss, and `durable_through()` lied). Added the durable write pattern (`write_parquet` `sync_all`, `write_segment_parquet` fsyncs the day-partition dir after rename, `persist_manifest` temp → fsync → rename → fsync-dir). Order becomes segment-durable → manifest-durable → WAL-truncate.
- **HIGH 2 — compaction deleted source segments before the merged manifest was durable** (crash → `cleanup_orphans` deletes the unreferenced merged file → total loss). `compact_partition` collects sources into a `deferred_deletes` vec; `compact()` persists+fsyncs the manifest (pointing at the merged segments) and only then deletes the sources.
- **HIGH 3 — `seal()` emptied buffers up front**; a mid-seal write error followed by continued operation let a later seal truncate the un-sealed rows' WAL frames → loss. Fixed: the six `write_*_segment` fns take `rows: &mut [X]` instead of consuming `Vec<X>` (they only needed mutability to sort in place), so `seal` keeps ownership across the writes. On **Ok** it commits exactly as before (the concurrency-correct `mem::take` success path unchanged); on **Err** it hands the un-sealed rows back with `prepend_rows` (ahead of anything ingested concurrently while the seal lock was released, since those carry higher LSNs) and restores their `buffer_bytes` accounting, then returns the error. Segments already written this pass become orphans that `cleanup_orphans` reclaims on reopen.
- **MEDIUM 4 — retention deleted files before persisting the manifest** (crash → manifest references missing files → every query fails). Reordered `retain()`: update in-memory segment lists → persist manifest → delete files.
- **MEDIUM 5 — `compact()` held the `inner` mutex across heavy Parquet I/O** (foreground stall; an Arrow panic under the lock poisons the mutex → all `.lock().unwrap()` panic → DB bricked). Fixed: `compact()` (1) snapshots the segment lists under the lock (a cheap `Vec<SegmentRef>` clone of the metadata, not Parquet data) then releases it; (2) runs all Parquet I/O off-lock (a panic there neither poisons the mutex nor corrupts state — it unwinds with `inner` untouched); (3) re-takes the lock only to reconcile and persist. `reconcile_segments` drops the compacted-away sources, adds the merged results, and keeps any segment a concurrent seal appended while compaction ran off-lock; (4) deletes sources after the manifest is durable. `retain()` is left holding the lock (acceptable: only stat + unlink + manifest write, no Parquet read/concat/sort, so no Arrow-panic/poison path and only a brief hold).
- **LOW 6** — WAL `checksum` covers the `len` prefix; `read_frames` enforces strictly-increasing LSNs (stops at a non-monotonic, checksum-passing region).
- **LOW 7** — `Wal::append` rejects payloads `> u32::MAX` (was a silent `as u32` truncation → tail loss).
- **LOW 8** — retention cutoff uses `i64::try_from(...).unwrap_or(i64::MAX)` (was `as i64`, wrapped for absurd `max_age`); manifest `rows` parsed as `u64` directly.

### Opt-in background maintenance scheduler (§5/§10.2)

`Maintenance` config enum in `imbh-core` (`Manual` default | `Background(Duration)`), plumbed through `DbBuilder::maintenance(...)`. On `open()`, `Background(interval)` on a non-in-memory DB spawns one owned thread running `run_maintenance(Weak<Inner>, interval)`: it polls at `min(interval, 1s)` (clamped ≥5ms) so it notices `close()` / handle-drop promptly, and every `interval` calls `storage.seal()` + `storage.retain()`. The thread holds only a `Weak<Inner>`, so dropping every `Db` clone (or `close()` flipping the `closed` flag) stops it — preserving the "no background threads unless opted in" guarantee. A `Weak`-handle + `closed`-flag bounds a library-owned thread's lifetime to the DB handle without a shutdown channel: no join handle to store, no `Drop` impl needed on `Inner`. (`Maintenance::{Background, Runtime}` — the `Runtime` variant uses `Handle::spawn` instead of an owned OS thread, mirroring the async-ingest worker.)

### Opt-in asynchronous ingest (`Ingest::Async`)

An opt-in async-ingest mode decouples the ingest call from WAL I/O, fsync latency, and buffer-lock contention, while the default stays fully inline. Config in `imbh-core::config`: `Ingest::{Sync, Async{handle, capacity, overflow}}` + `Overflow::{Block, Fail, DropOldest}`, exposed via `DbBuilder::ingest(..)`. The queue lives in `crates/imbh/src/ingest_queue.rs` (a bounded `Mutex<VecDeque<IngestJob>>` + two `tokio::sync::Notify`, single-consumer worker); the worker loop `run_ingest_worker` lives in `lib.rs` since it needs the private `Db::storage`.

- The `async` ingest facade was previously a no-op offload — decode/WAL/encode all ran inline on the caller despite `async fn`. In the new mode, decode is deliberately kept on the caller so `accepted` and malformed-body errors stay synchronous; only WAL append + Arrow encode + buffer push move to the worker. The async receipt is a *queued ack* (`lsn: None`, `durable: false`, `is_queued() == true`); the per-call `durable_through() >= receipt.lsn` handshake is unavailable, so durability is confirmed globally via `flush()` / `close()`. See "`Lsn` is `NonZero<u64>`" below for why the queued case is `None` rather than a zero sentinel.
- Reused, not rebuilt: `Error::queue_full` / `is_backpressure` fit the `Overflow::Fail` path; the `Weak<Db>` worker + `close()`-joins-the-handle lifecycle was copied from `run_maintenance_async`; `Storage::ingest*` needed zero changes (the worker calls them with `sync_now = true`, so `WalMode::Always` durability is preserved, just moved off the caller). No new crate — only tokio's `sync` feature (footprint-neutral; facade crate count unchanged at 275).
- Worker model is `Handle::spawn`, never an owned OS thread (per the planned Go binding, which drives tokio with host threads). `Ingest::Async` is ignored for in-memory / read-only DBs.
- Lost-wakeup discipline: both the worker park and the `Overflow::Block` producer use the pinned `Notify::notified()` + `.enable()` idiom *before* the under-lock state check, so a notify racing the check is captured on the future rather than dropped. The single-consumer invariant keeps this tractable.
- `close()` ordering matters for no-loss shutdown: the ingest worker is drained + awaited *before* the final seal (and before the maintenance-worker join), so every enqueued job lands in the buffer and is sealed. Dropping the last `Db` without `close()` discards in-flight jobs (the same `Weak` lifecycle caveat maintenance carries).
- `stats()` gained `ingest_queue_depth` / `ingest_dropped` / `ingest_errors`.

### Group-commit fsync for the async-ingest worker

`Storage::group_commit()` (imbh-storage) is a single `wal.sync()` that advances `durable_lsn` to `buffer_max_lsn`, a no-op unless `WalMode::Always`. The async worker (`run_ingest_worker`) appends each drained job with `sync_now = false` and calls `group_commit()` **once per drained burst** instead of fsyncing per job.

- The drain-then-commit shape falls out of the existing worker loop: it already drains the queue in a `while let Some(job) = chan.pop()` burst before parking, so group-commit is one call after that inner loop, guarded by a `processed > 0` count and placed *before* the `is_closed()` break so a clean `close()` both drains and commits. The burst boundary is the natural commit boundary — no new synchronization.
- Reading `buffer_max_lsn` before the fsync is a safe durability lower bound: `ingest` does `w.append(frame)` (write_all to the file) *before* publishing `inner.buffer_max_lsn`, so any LSN a reader observes is already in the file and covered by `sync_data`. A concurrent append that lands after the read is flushed too but simply not advertised until the next commit — conservative, never overclaims.
- `sync_now=false` changes nothing for `Interval` / `Off`: the pre-existing `wal_append_assign` only fsynced under `sync_now && WalMode::Always`, so flipping the worker to `sync_now=false` alters only the `Always` path; `group_commit` short-circuits for the others.
- Durability is still monotonic, just coarser-grained: under `Always`+async, `durable_through` advances at burst boundaries rather than per job. This is fine because the async receipt is a queued ack with no per-call lsn to compare against.

### `Lsn` is `NonZero<u64>`; "no position" is `Option<Lsn>`

`Lsn` is **`pub type Lsn = std::num::NonZero<u64>;`** (`imbh-core/src/ids.rs`), not a newtype over `u64`. Zero is therefore not representable, and every "there is no LSN here" case is spelled `Option<Lsn>` = `None`. This closed the async-receipt footgun where a queued ack carried `lsn = Lsn(0)` and a host keying off `receipt.lsn` for a `durable_through()` handshake silently compared against a fake position.

- The invariant that makes this sound: a real assigned LSN is always ≥ 1. `Inner::next_lsn` starts at 1 (fresh `next_lsn: 1`; reopened `watermark.max(max_lsn) + 1` / `watermark + 1`) and only increments, so 0 never collides with a valid LSN. A `debug_assert!(lsn >= 1)` at LSN allocation enshrines it, and the three `imbh-storage` ingest wrappers construct `Lsn::new(lsn).expect("assigned LSN is >= 1")`.
- Two public signatures carry the `Option`: `Db::durable_through() -> Option<Lsn>` (`None` until anything is durable — the watermark legitimately starts at 0, which is exactly why the newtype could not be made `NonZero` without this change) and `IngestReceipt.lsn: Option<Lsn>`.
- The `queued: bool` field was **dropped** in favour of `is_queued() { self.lsn.is_none() }` — one source of truth instead of a flag that could disagree with the position. `IngestReceipt` is now `(durable, lsn: Option<Lsn>)` with `synced()` / `queued()` constructors.
- Wire compatibility was preserved at the edges: `imbhd`'s `/stats` still emits a numeric `durable_lsn` via `stats.durable_lsn.map_or(0, |l| l.get())` (0 = nothing durable), and `/v1/*` ingest JSON reports `r.is_queued()`.
- Migration mechanics: `Lsn(x)` → `Lsn::new(x)` (an `Option`), `.0` → `.get()`. The alias had no custom impls or serde, so the swap was clean. Documented in `ARCHITECTURE.md` §10.4 (ids & enums) and §10.5 (ingest).
- The generalizable move: when a sentinel value is invalid in *every* position it can occupy, encode the invalidity in the type (`NonZero`) and let `Option` carry the absence, rather than adding a companion boolean to make the sentinel legible.

### Cross-cutting durability invariants

- **One crash-safe-append idiom, used in three places.** Length + XXH3 framing that stops at the first torn/checksum-failing frame is the standard for append-only durable files: the WAL frames, the incremental WAL cursor's resume-from-clean-boundary, and the manifest delta log. A half-written trailing record is *always* "the op didn't happen durably", and the data it would have committed replays from the still-unreclaimed WAL. New durability code should reach for this shape.
- **Durability ordering is the invariant to protect, not the file format.** "Make the metadata edit durable *before* reclaiming the WAL / deleting files" is what makes every crash point (torn append, mid-roll, reader-during-roll) reduce to "replay from the WAL".
- **The reader model rests on two *independent* correctness mechanisms — keep them separate.** (a) The manifest re-check bracket (watermark-stable ⇒ segments `≤W` and WAL tail `>W` are disjoint + complete) guards the seal boundary; (b) the `collect_with_stats` read-during-delete retry (re-derive + retry if a snapshotted segment path vanished) guards retention/compaction file deletes.
- **Prefer diff-internally over hand-authored deltas.** The manifest writer kept the callers' full-state contract and computed the delta by mechanical set-diff (segments immutable + uniquely named ⇒ path identity stable), making the change correct-by-construction.

## Files

- `crates/imbh-storage/src/wal.rs` — the WAL: frame `(len, xxh3, lsn, signal, payload)` append/read, `read_frames` (stop-on-corruption, monotonic LSN check), `encode_frame`, `Wal::append` (rejects `> u32::MAX`), `Wal::truncate_below`, `Wal::sync`.
- `crates/imbh-storage/src/manifest.rs` — the v2 append-only manifest: `CURRENT` + `MANIFEST-<NNNNNN>` framed delta log, `ManifestWriter` (`persist`, checkpoint roll at `CHECKPOINT_BYTES` = 256 KiB), `manifest::read`, legacy migration, the `Manifest` type (~560 lines incl. tests).
- `crates/imbh-storage/src/lib.rs` — `Storage`: `open` (with `load_manifest` + `cleanup_orphans`), `seal`, `retain`, `compact` / `compact_partition` / `reconcile_segments`, `replay`, `group_commit`, `write_*_segment` (six, `rows: &mut [X]`), `write_parquet` / `write_segment_parquet`, `persist_manifest`, `snapshot`, `read_disk_snapshot` / `read_disk_snapshot_incremental` (`WalTailCursor`), `logs_batch_to_index_rows`, reports (`RetentionReport`, `CompactionReport`, `MaintenanceReport`).
- `crates/imbh-core` — config types: `WalMode`, `Retention`, `Maintenance` (`Manual` / `Background(Duration)` / `Runtime`), `Ingest` (`Sync` / `Async`), `Overflow`; `Error::queue_full` / `is_backpressure`; `IngestReceipt` (`durable`, `lsn: Option<Lsn>`, `is_queued()`); `ids.rs` — `Lsn = std::num::NonZero<u64>`.
- `crates/imbh/src/lib.rs` — facade: `Db::builder(..).wal/.retention/.maintenance/.ingest`, `Db::maintain` / `compact` / `durable_through` / `segment_files`, replay orchestration (decode via `imbh-otlp` → `storage.replay`), `run_maintenance` / `run_maintenance_async`, `run_ingest_worker`, `close()` shutdown ordering.
- `crates/imbh/src/ingest_queue.rs` — bounded single-consumer async ingest queue (`Mutex<VecDeque<IngestJob>>` + two `tokio::sync::Notify`).
- `crates/imbh-index` — logs Tantivy `.tidx`: `build_logs_index` (arrow-free; consumes minimal `LogRow`s).

## Test Coverage

Run the storage suite: `cargo test -p imbh-storage`. Full gate: `cargo test --workspace` (325 passed / 0 failed at the manifest-rework milestone). Crash-point tests: `cargo test --features fault-injection --test crash_points`.

- WAL + replay: 4 storage WAL tests + 1 facade recovery test (M1a).
- `seal_truncates_wal` — five ingests grow the WAL; `seal()` (watermark = 5) empties it to 0 bytes; a post-seal ingest (lsn 6) regrows it; reopen replays exactly the one unsealed frame.
- `open_cleans_orphan_segments` — seals a real segment, plants an orphan parquet + sidecar + a stray `.tmp`, reopens → orphans gone, manifest-referenced segment survives.
- `compaction_merges_and_survives_reopen` — two same-day segments merge to one, sources deleted, all rows preserved, reopen loads only the merged segment.
- `compact_merges_histogram_segments` — two histogram segments in one UTC day merge to one (`segments_merged >= 2`), row count and `List` bucket columns intact.
- `failed_seal_restores_buffer` — blocks the seal deterministically (a regular file where the `logs/` partition dir must be created → `create_dir_all` fails), asserts `seal()` errors, asserts the 2 rows are still buffered, then unblocks and confirms they seal.
- `reconcile_preserves_concurrent_segments` — sources A/B dropped, concurrently-added C preserved, merged M added.
- `torn_trailing_frame_is_ignored`, `open_cleans_stray_manifest_files_from_an_interrupted_roll` — manifest delta-log crash safety (4 manifest module tests + 1 storage cleanup test; the roll test forces `CHECKPOINT_BYTES` by setting `log_bytes` directly).
- `group_commit_batches_fsync_and_advances_durable` (storage unit) + `async_ingest_group_commit_advances_durable_under_always` (E2E; bounded-yields the current-thread runtime until the worker's group-commit advances `durable_through`).
- Async ingest: `async_ingest_fail_policy_sheds_when_full`, `..._drop_oldest_counts_evictions` (deterministic overflow on `ct_rt()`); `crates/imbh/tests/async_ingest.rs` proves drain-on-close across all three signals via reopen-and-count.
- `background_maintenance_auto_seals` — opens with a 20ms interval, ingests one log, polls (with timeout) until a segment auto-appears.
- `--features fault-injection --test crash_points` — `before_manifest` / `after_manifest` crash points (unchanged by the manifest rework).

## Pitfalls

- **The seal watermark must be `buffer_max_lsn` (rows actually in the buffer), not the highest LSN handed out.** Otherwise a concurrent ingest that got an LSN but has not appended its rows is marked sealed → silent data loss on replay. Assign LSN + append rows under one `Inner` lock, WAL append inside the critical section.
- **`WalMode::Interval` fsync is approximate** — it happens on `flush` / `close` / `Always`, not on a background timer.
- **Async-mode receipts are queued acks** (`lsn: None`, `durable: false`, `is_queued()`) — there is no per-call `durable_through() >= receipt.lsn` handshake; confirm durability via `flush()` / `close()`.
- **`Lsn` is `NonZero<u64>`, so `Lsn(0)` does not exist.** Anything that meant "no LSN yet" is `Option<Lsn>` = `None` — including `Db::durable_through()`, which returns `None` (not a zero watermark) until something is durable. Do not reintroduce a numeric zero sentinel; if a wire format needs one, map it at the edge (`map_or(0, |l| l.get())`).
- **Dropping the last `Db` without `close()` discards in-flight async-ingest jobs** (and stops background maintenance) — the `Weak` lifecycle caveat. Call `close()` for no-loss shutdown; the ingest worker drains before the final seal.
- **Segment on-disk size must include the `.tidx` sidecar dir** (recursive `dir_size`), or retention under-counts and keeps too much.
- **Rebuild the logs index from the merged, time-sorted batch, not the source rows** — reading Parquet back and re-sorting guarantees the index `row` ordinal matches the merged Parquet's row order.
- **New metric tables that plug into the shared `metric_segments` map must be picked up by every code path that iterates it** — compaction originally iterated only `SCALAR_METRIC_TABLES` and silently skipped `metrics_histogram` / `metrics_exp_histogram` / `metrics_summary`. Retention/snapshot/stats use `.values()` and were covered.
- **Durability ordering must never be reordered**: segment-durable → manifest-durable → WAL-truncate / file-delete. Deleting or truncating before the replacement is durable was the root of three HIGH/MEDIUM data-loss bugs.
- **Do not hold the `inner` mutex across heavy Parquet I/O** — an Arrow panic (`concat_batches` / `take` / sort) under the lock poisons the mutex and bricks the DB. Snapshot metadata under the lock, do I/O off-lock, re-lock to reconcile+persist.
- **After a WAL `rename`, reopen the append handle on the compacted path** — the live `Wal` still holds a `File` on the old inode.
- **File deletion must tolerate already-absent paths** (`NotFound` → ok): a crash between manifest rewrite and file unlink, or a missing sidecar, must not fail a later retention/cleanup pass.
- **Avoid `as` casts on lengths/timestamps**: `Wal::append` payloads `> u32::MAX` and retention `max_age` conversions must use checked/`try_from` conversions, not silent-truncating `as`.
