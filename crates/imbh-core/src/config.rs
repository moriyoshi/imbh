//! DB configuration knobs (ARCHITECTURE.md §10.2). Honored today: [`MemoryBudget`] (query pool),
//! [`Compression`] (segment codec), [`WalMode`] (fsync policy), [`Retention`]
//! (age + disk-budget), and [`Promote`] (attribute keys lifted to typed columns). The
//! maintenance-scheduler knob lands later.

use std::time::Duration;

/// Attribute keys promoted to typed columns at rest (ARCHITECTURE.md §6.1). Each key becomes a
/// nullable `Dictionary(Int32,Utf8)` column appended to **every** signal's schema (uniform across
/// logs/spans/metrics), populated at buffer-encode time from the row's canonical-JSON scopes
/// (record `attributes` → `resource` → `scope`, most-specific wins; non-string values become NULL).
///
/// The key **also stays inside the canonical-JSON blob** — the JSON remains the single source of
/// truth, so `json_get_str`, external `json_extract`, and the reference label evaluators keep
/// working unchanged. The column is a pushdown / zero-copy *accelerator*, not a relocation: a
/// promoted-label filter can hit a real dictionary column instead of a `json_get_str` scan.
///
/// Changing the set is backward-compatible: segments sealed before a key was promoted simply lack
/// the column and are null-filled at query time (the `coerce` schema-evolution path). An empty
/// `Promote` (the default) adds no columns and costs nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Promote {
    keys: Vec<String>,
}

impl Promote {
    /// Promote the given attribute keys. Duplicates are dropped, first-occurrence order preserved
    /// (column order is stable across restarts for a given set). Keys that collide with a built-in
    /// column name are filtered later, at schema construction, so the promoted set stays disjoint
    /// from the fixed schema regardless of what is passed here.
    pub fn new(keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut out: Vec<String> = Vec::new();
        for k in keys {
            let k = k.into();
            if !out.contains(&k) {
                out.push(k);
            }
        }
        Promote { keys: out }
    }

    /// The promoted keys, in column order.
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// `true` when nothing is promoted (the default) — schemas are then untouched.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Caps buffer + query pool + writer heap under one budget (OVERVIEW.md §2 / ARCHITECTURE.md §10.2). M0 uses it
/// only to size the DataFusion memory pool and the buffer seal threshold.
#[derive(Debug, Clone, Copy)]
pub struct MemoryBudget {
    total_bytes: usize,
}

impl MemoryBudget {
    pub fn total(bytes: usize) -> Self {
        MemoryBudget { total_bytes: bytes }
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

impl Default for MemoryBudget {
    fn default() -> Self {
        // 128 MiB, matching the Appendix B sketch.
        MemoryBudget {
            total_bytes: 128 << 20,
        }
    }
}

/// Segment compression codec (ARCHITECTURE.md §10.2). zstd is the default; lz4 is the pure-Rust
/// fallback for `zstd`-off builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Lz4,
    Zstd(i32),
}

impl Default for Compression {
    fn default() -> Self {
        Compression::Zstd(3)
    }
}

/// How a [`Db`] opens a directory (ARCHITECTURE.md §5: single writer, many readers).
///
/// - `ReadWrite` (default) — the one writer. Open acquires an exclusive advisory lock on the
///   directory's `writer.lock`; a second `ReadWrite` open (this process or another) fails with
///   [`crate::Error::lock_held`]. Released automatically on drop / process exit.
/// - `ReadOnly` — a reader. Takes no lock, never mutates the directory, and answers queries from a
///   point-in-time snapshot of the manifest's segments unioned with the writer's live WAL tail
///   (near-real-time visibility). Any write (ingest/flush/maintain/compact) returns
///   [`crate::Error::read_only`]. Many `ReadOnly` opens may coexist with the single writer.
///
/// [`Db`]: https://docs.rs/imbh
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    #[default]
    ReadWrite,
    ReadOnly,
}

/// WAL fsync policy (ARCHITECTURE.md §7/§10.2).
///
/// - `Always` — fsync every ingest before returning (durable receipts).
/// - `Interval(d)` — fsync at most every `d`. M1 fsyncs opportunistically on `flush`/`close`
///   (no background timer thread yet — the embedder "no background threads" guarantee, §5);
///   a timer-driven flusher is a follow-up.
/// - `Off` — never fsync inline; durability follows the OS. The WAL frames are still written,
///   so a clean restart still replays them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalMode {
    Off,
    Interval(std::time::Duration),
    Always,
}

impl Default for WalMode {
    fn default() -> Self {
        // ARCHITECTURE.md §7 default: interval(1s).
        WalMode::Interval(Duration::from_secs(1))
    }
}

/// Retention policy (ARCHITECTURE.md §7/§10.2). Data is immutable; deletion happens only here. Both
/// bounds are optional and combine: a segment is dropped if it is older than `max_age` **or**
/// if the total on-disk size exceeds `max_disk_bytes` (oldest segments first).
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    max_age: Option<Duration>,
    max_disk_bytes: Option<u64>,
}

impl Retention {
    /// No retention — keep everything (the default).
    pub fn none() -> Self {
        Retention {
            max_age: None,
            max_disk_bytes: None,
        }
    }

    /// Drop segments older than `n` days.
    pub fn days(n: u64) -> Self {
        Retention {
            max_age: Some(Duration::from_secs(n * 86_400)),
            max_disk_bytes: None,
        }
    }

    /// Also cap the total on-disk segment size (oldest dropped first).
    pub fn max_disk_bytes(mut self, bytes: u64) -> Self {
        self.max_disk_bytes = Some(bytes);
        self
    }

    pub fn max_age(&self) -> Option<Duration> {
        self.max_age
    }

    pub fn disk_budget(&self) -> Option<u64> {
        self.max_disk_bytes
    }
}

impl Default for Retention {
    fn default() -> Self {
        Retention::none()
    }
}

/// Maintenance policy (ARCHITECTURE.md §5/§10.2) — the knob behind "no background threads unless opted
/// in". `Manual` means the host must call `db.maintain()`; `Background(interval)` spawns one owned
/// thread that periodically seals + applies retention; `Runtime(handle, interval)` runs that same
/// loop on a host-provided tokio runtime instead of owning an OS thread. Both `Background` and
/// `Runtime` also seal promptly the moment the buffer crosses its byte threshold — the `interval`
/// only governs the periodic seal + retention.
///
/// Carrying a [`tokio::runtime::Handle`] makes this enum non-`Copy` (it stays `Clone`).
#[derive(Debug, Clone, Default)]
pub enum Maintenance {
    #[default]
    Manual,
    Background(Duration),
    /// Schedule the maintenance loop onto a host-provided tokio runtime (no owned OS thread), at the
    /// given periodic seal + retention `interval`. The buffer-byte seal fires on every tick
    /// regardless of `interval`. Ignored for in-memory DBs.
    Runtime(tokio::runtime::Handle, Duration),
}

/// Ingest execution policy (ARCHITECTURE.md §5/§10.5) — the second knob behind "no background threads
/// unless opted in". `Sync` (the default) runs the whole ingest inline on the caller's thread, exactly
/// as today: decode → WAL append (+fsync) → buffer push, so the receipt is immediate and, under
/// [`WalMode::Always`], durable. `Async` hands the WAL append + buffer push to one background worker
/// task on a host-provided tokio runtime, so the caller returns as soon as the decoded rows are
/// *enqueued* — the receipt is then a *queued* acknowledgement (`accepted` is real, but `lsn`/`durable`
/// are not yet known; confirm durability globally via `flush()`/`close()`).
///
/// The protobuf decode always runs on the caller (so a malformed body and the `accepted` count are
/// still reported synchronously); only the WAL + buffer work is offloaded. The worker is a
/// `tokio::runtime::Handle::spawn` task, never an owned OS thread — the same host-runtime model as
/// [`Maintenance::Runtime`], so an FFI host (e.g. the Go binding) drives it with its own threads.
///
/// Carrying a [`tokio::runtime::Handle`] makes this enum non-`Copy` (it stays `Clone`). Ignored for
/// in-memory and read-only DBs (they have no writer worker).
#[derive(Debug, Clone, Default)]
pub enum Ingest {
    /// Inline ingest on the caller's thread (today's behavior); no background worker.
    #[default]
    Sync,
    /// Offload WAL append + buffer push to one background worker task on `handle`. `capacity` bounds
    /// the in-flight job queue (decoded OTLP requests); `overflow` picks what happens when it is full.
    Async {
        handle: tokio::runtime::Handle,
        capacity: usize,
        overflow: Overflow,
    },
}

/// What the async ingest queue does when it is full (ARCHITECTURE.md §10.5). `Block` is the safe
/// default (no data loss, natural backpressure); `Fail` and `DropOldest` are load-shedding policies for
/// hosts that would rather shed than stall.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overflow {
    /// The async `ingest_otlp_*` call awaits until the worker frees a slot (natural backpressure). The
    /// non-blocking `try_ingest_otlp_*` call cannot await, so under this policy it fails fast instead.
    #[default]
    Block,
    /// Return [`crate::Error::queue_full`] immediately (a backpressure error) when the queue is full.
    Fail,
    /// Evict the oldest un-processed job to make room, then enqueue the new one (load-shed the tail of
    /// the backlog; the eviction is counted in `stats().ingest_dropped`).
    DropOldest,
}

/// Read-only snapshot-refresh policy (ARCHITECTURE.md §5). A read-only handle answers each query from
/// a point-in-time snapshot (manifest segments ∪ live WAL tail). Rebuilding that snapshot means
/// re-reading the writer's newly appended WAL — cheap per byte (the reader tracks per-segment offsets
/// and scans only what is new), but a busy reader issuing many queries can amortize it further by
/// reusing one snapshot for a short window. This knob trades a bounded amount of staleness for that.
/// Ignored for read-write and in-memory handles (they query their own live buffers). See also
/// `allow_stale_reads`, which is about a WAL-*off* writer, a different concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Refresh {
    /// Rebuild the snapshot on every query — near-real-time visibility (the default, unchanged
    /// behavior). Still incremental: each rebuild scans only WAL bytes appended since the last.
    #[default]
    OnQuery,
    /// Reuse a cached snapshot for up to this duration; the first query past it rebuilds. Bounds
    /// staleness to roughly `d` while collapsing a burst of queries onto one WAL scan.
    Ttl(Duration),
    /// Never auto-rebuild — queries see a fixed snapshot until an explicit `Db::refresh()`. Gives a
    /// reader a stable view across many queries (e.g. a dashboard render) with no implicit drift.
    Manual,
}
