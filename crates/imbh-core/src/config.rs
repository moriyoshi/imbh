//! DB configuration knobs (ARCHITECTURE.md §10.2). Honored today: [`MemoryBudget`] (query pool),
//! [`Compression`] (segment codec), [`WalMode`] (fsync policy), [`Retention`]
//! (age + disk-budget), [`Promote`] (attribute keys lifted to typed columns), [`Maintenance`] (who
//! runs the scheduler), [`FlushPolicy`] (when that scheduler seals the buffer) and [`Duplicates`]
//! (what happens to two metric points sharing a series and a timestamp).

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use crate::error::{Error, Result};

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
/// Changing the set between runs is safe for **queries**: segments sealed before a key was promoted
/// lack the column and are null-filled at query time (the `coerce` schema-evolution path), and the
/// query layer falls back to reading that key out of the JSON blob exactly on those rows, so a filter
/// or group-by on a newly promoted key still sees its history. An empty `Promote` (the default) adds
/// no columns and costs nothing.
///
/// *(Before 0.7.0 this was not true. The null-fill was real but the query layer emitted only the
/// column form, so a filter on a newly promoted key silently matched nothing on every pre-promotion
/// segment — this doc previously called that "backward-compatible". See `SqlParams::attr_field`.)*
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
/// - `Interval(d)` — fsync at most every `d`, on the flush scheduler's clock. Because the scheduler
///   is opt-in ([`Maintenance::Background`] / [`Maintenance::Runtime`] — the embedder "no background
///   threads" guarantee, §5), a `Manual` DB fsyncs only opportunistically, on `flush`/`close`.
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

    /// Rebuild a policy from its two bounds — the inverse of [`Self::max_age`] / [`Self::disk_budget`],
    /// so the policy can be persisted and read back rather than living only in a builder call.
    ///
    /// The policy is **durable database state**: a housekeeper process that applies retention must
    /// apply the *host's* policy, not one invented from its own flags, and two handles on one
    /// directory must not disagree about when data is deleted. Same reasoning as [`Promote`].
    pub fn from_parts(max_age: Option<Duration>, max_disk_bytes: Option<u64>) -> Self {
        Retention {
            max_age,
            max_disk_bytes,
        }
    }
}

impl PartialEq for Retention {
    fn eq(&self, other: &Self) -> bool {
        self.max_age == other.max_age && self.max_disk_bytes == other.max_disk_bytes
    }
}

impl Eq for Retention {}

impl Default for Retention {
    fn default() -> Self {
        Retention::none()
    }
}

/// Maintenance policy (ARCHITECTURE.md §5/§10.2) — the knob behind "no background threads unless opted
/// in". It picks **who runs the scheduler**: `Manual` means the host must call `db.maintain()`;
/// `Background(interval)` spawns one owned thread; `Runtime(handle, interval)` runs that same loop on a
/// host-provided tokio runtime instead of owning an OS thread.
///
/// The `interval` is the **retention** cadence (the periodic `retain()` pass). **When** the buffer is
/// sealed is a separate decision, owned by [`FlushPolicy`] — and when the host sets no policy, the
/// same `interval` doubles as the periodic seal cadence, which is the historical behavior.
///
/// Carrying a [`tokio::runtime::Handle`] makes this enum non-`Copy` (it stays `Clone`).
#[derive(Debug, Clone, Default)]
pub enum Maintenance {
    #[default]
    Manual,
    Background(Duration),
    /// Schedule the maintenance loop onto a host-provided tokio runtime (no owned OS thread), at the
    /// given retention `interval`. Sealing follows [`FlushPolicy`], evaluated every policy tick
    /// regardless of `interval`. Ignored for in-memory DBs.
    Runtime(tokio::runtime::Handle, Duration),
}

/// How big the mutable buffer may get before the flush scheduler seals it — the *size-based* trigger
/// of a [`FlushPolicy`] (ARCHITECTURE.md §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FlushSize {
    /// Derive the threshold from the [`MemoryBudget`] (a quarter of it, floored at 8 MiB) — the
    /// default, and what the engine has always used.
    #[default]
    Budget,
    /// An explicit buffered-heap threshold in bytes.
    Bytes(usize),
    /// No size-based sealing at all (time/row/WAL triggers, if any, still apply).
    Off,
}

/// When the flush scheduler turns the mutable buffer into an immutable segment (ARCHITECTURE.md
/// §5/§7/§10.2).
///
/// A [`Db`] never seals on its own: sealing happens on an explicit `flush()`/`maintain()`/`close()`,
/// or on the opt-in scheduler thread/task chosen by [`Maintenance`]. This type is what that scheduler
/// consults — the strategies are **independent triggers, OR-ed together**, so a policy can be purely
/// periodic, purely size-based, or any mix:
///
/// - **Periodic** — [`every`](Self::every): seal every `d`, regardless of how little is buffered.
/// - **Size (heap)** — [`at_buffer_bytes`](Self::at_buffer_bytes) / [`size`](Self::size): seal once the
///   buffered rows hold at least `n` bytes. Defaults to [`FlushSize::Budget`].
/// - **Size (rows)** — [`at_buffer_rows`](Self::at_buffer_rows): seal once at least `n` rows are
///   buffered across all tables.
/// - **WAL size** — [`at_wal_bytes`](Self::at_wal_bytes): seal once the on-disk WAL reaches `n` bytes.
///   Sealing is what lets the WAL be reclaimed, so this bounds WAL growth directly.
/// - **Idle** — [`after_idle`](Self::after_idle): seal once nothing has been ingested for `d` (and
///   something is buffered). Lands a quiet workload's tail in Parquet without a short periodic timer.
///
/// [`tick`](Self::tick) sets how often the triggers are evaluated (default 1s); the periodic trigger is
/// rounded up to it.
///
/// The [`Default`] is [`FlushSize::Budget`] with no trigger of its own beyond that: a host that never
/// calls `DbBuilder::flush` keeps the historical behavior, where the [`Maintenance`] interval supplies
/// the periodic seal. [`manual`](Self::manual) disables every trigger, so only explicit `flush()`
/// seals.
///
/// A policy also parses from a compact spec string (`"interval=5s,wal=64MiB"`, or `"manual"`) via
/// [`FromStr`], which is how a host exposes it as one config value; [`fmt::Display`] renders the same
/// syntax back.
///
/// [`Db`]: https://docs.rs/imbh
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlushPolicy {
    size: FlushSize,
    interval: Option<Duration>,
    rows: Option<u64>,
    wal_bytes: Option<u64>,
    idle: Option<Duration>,
    tick: Duration,
}

impl FlushPolicy {
    /// The scheduler's default evaluation cadence.
    pub const DEFAULT_TICK: Duration = Duration::from_secs(1);
    /// Ticks are clamped into this range: fast enough to honor a sub-second trigger, slow enough that
    /// a mistyped `tick=0` cannot spin the scheduler.
    const MIN_TICK: Duration = Duration::from_millis(5);
    const MAX_TICK: Duration = Duration::from_secs(60);

    /// No automatic sealing: only `flush()` / `maintain()` / `close()` seal. A scheduler configured
    /// with this policy still applies retention on the [`Maintenance`] interval and still fsyncs the
    /// WAL under [`WalMode::Interval`].
    pub fn manual() -> Self {
        FlushPolicy {
            size: FlushSize::Off,
            interval: None,
            rows: None,
            wal_bytes: None,
            idle: None,
            tick: Self::DEFAULT_TICK,
        }
    }

    /// Seal every `d` (the periodic strategy). Keeps the default size trigger; chain the other
    /// setters to add more.
    pub fn periodic(d: Duration) -> Self {
        FlushPolicy::default().every(d)
    }

    /// Seal once the buffered rows hold at least `bytes` (the size-based strategy), with no periodic
    /// timer of its own.
    pub fn size_based(bytes: usize) -> Self {
        FlushPolicy::default().at_buffer_bytes(bytes)
    }

    /// Add/replace the periodic trigger: seal every `d`.
    pub fn every(mut self, d: Duration) -> Self {
        self.interval = Some(d);
        self
    }

    /// Set the size-based trigger explicitly (including [`FlushSize::Off`]).
    pub fn size(mut self, size: FlushSize) -> Self {
        self.size = size;
        self
    }

    /// Add/replace the size-based trigger: seal at `bytes` of buffered heap.
    pub fn at_buffer_bytes(self, bytes: usize) -> Self {
        self.size(FlushSize::Bytes(bytes))
    }

    /// Add/replace the row-count trigger: seal at `rows` buffered rows (summed across tables).
    pub fn at_buffer_rows(mut self, rows: u64) -> Self {
        self.rows = Some(rows);
        self
    }

    /// Add/replace the WAL-size trigger: seal once the on-disk WAL reaches `bytes`. Sealing advances
    /// the watermark, which is what allows the covered WAL to be reclaimed.
    pub fn at_wal_bytes(mut self, bytes: u64) -> Self {
        self.wal_bytes = Some(bytes);
        self
    }

    /// Add/replace the idle trigger: seal once nothing has been ingested for `d` and the buffer is
    /// non-empty.
    pub fn after_idle(mut self, d: Duration) -> Self {
        self.idle = Some(d);
        self
    }

    /// How often the triggers are evaluated. Clamped to [5ms, 60s]; a `tick` longer than the periodic
    /// interval is additionally clamped to it by [`Self::effective_tick`].
    pub fn tick(mut self, d: Duration) -> Self {
        self.tick = d.clamp(Self::MIN_TICK, Self::MAX_TICK);
        self
    }

    pub fn size_trigger(&self) -> FlushSize {
        self.size
    }

    pub fn interval(&self) -> Option<Duration> {
        self.interval
    }

    pub fn buffer_rows(&self) -> Option<u64> {
        self.rows
    }

    pub fn wal_bytes(&self) -> Option<u64> {
        self.wal_bytes
    }

    pub fn idle(&self) -> Option<Duration> {
        self.idle
    }

    /// The scheduler's sleep granularity: the configured tick, never longer than the periodic
    /// interval (so `interval=200ms` is honored without also setting `tick`), and never outside
    /// [5ms, 60s].
    pub fn effective_tick(&self) -> Duration {
        let mut tick = self.tick;
        if let Some(interval) = self.interval {
            tick = tick.min(interval);
        }
        if let Some(idle) = self.idle {
            tick = tick.min(idle);
        }
        tick.clamp(Self::MIN_TICK, Self::MAX_TICK)
    }

    /// `true` when no trigger is enabled — the scheduler then never seals on its own.
    pub fn is_manual(&self) -> bool {
        self.size == FlushSize::Off
            && self.interval.is_none()
            && self.rows.is_none()
            && self.wal_bytes.is_none()
            && self.idle.is_none()
    }

    /// `true` when a trigger needs the on-disk WAL size, which costs a directory scan — the scheduler
    /// skips that measurement otherwise.
    pub fn needs_wal_bytes(&self) -> bool {
        self.wal_bytes.is_some()
    }

    /// Fill in the periodic trigger from the [`Maintenance`] interval when the policy has none. Used
    /// at open for a host that configured maintenance but no explicit policy: sealing then keeps
    /// following the maintenance interval, as it did before this knob existed.
    pub fn or_interval(mut self, d: Duration) -> Self {
        if self.interval.is_none() {
            self.interval = Some(d);
        }
        self
    }

    /// Evaluate the size/row/WAL/idle triggers against a set of gauges. The periodic trigger is the
    /// scheduler's own bookkeeping, so it is not decided here.
    ///
    /// `size_threshold` is the budget-derived threshold the engine would use for [`FlushSize::Budget`];
    /// `wal_bytes` may be `None` when [`Self::needs_wal_bytes`] said not to measure it. `idle_for` is
    /// the time since the last ingest.
    pub fn triggered(
        &self,
        buffer_bytes: usize,
        buffer_rows: u64,
        wal_bytes: Option<u64>,
        idle_for: Duration,
        size_threshold: usize,
    ) -> bool {
        // Nothing buffered → nothing to seal. Cheap guard that also keeps the idle trigger from
        // re-firing on an empty buffer every tick of a quiet DB.
        if buffer_rows == 0 {
            return false;
        }
        let size_hit = match self.size {
            FlushSize::Budget => buffer_bytes >= size_threshold,
            FlushSize::Bytes(n) => buffer_bytes >= n,
            FlushSize::Off => false,
        };
        size_hit
            || self.rows.is_some_and(|n| buffer_rows >= n)
            || wal_bytes
                .zip(self.wal_bytes)
                .is_some_and(|(measured, limit)| measured >= limit)
            || self.idle.is_some_and(|d| idle_for >= d)
    }
}

impl Default for FlushPolicy {
    fn default() -> Self {
        FlushPolicy {
            size: FlushSize::Budget,
            interval: None,
            rows: None,
            wal_bytes: None,
            idle: None,
            tick: Self::DEFAULT_TICK,
        }
    }
}

/// Parse a flush-policy spec: comma-separated `key=value` pairs, or the single word `manual` (aliases
/// `off` / `none` / `never`).
///
/// Keys: `interval` / `every` (duration), `buffer` / `bytes` (size, or `budget` / `off`), `rows`
/// (integer), `wal` (size), `idle` (duration), `tick` (duration). Unknown keys are an error rather
/// than a silent no-op, so a typo in a deployment's config surfaces at startup.
///
/// ```
/// use imbh_core::FlushPolicy;
/// let p: FlushPolicy = "interval=5s,wal=64MiB".parse().unwrap();
/// assert_eq!(p.interval(), Some(std::time::Duration::from_secs(5)));
/// assert_eq!(p.wal_bytes(), Some(64 << 20));
/// assert!("manual".parse::<FlushPolicy>().unwrap().is_manual());
/// ```
impl FromStr for FlushPolicy {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let spec = s.trim();
        if spec.is_empty() {
            return Ok(FlushPolicy::default());
        }
        if matches!(
            spec.to_ascii_lowercase().as_str(),
            "manual" | "off" | "none" | "never"
        ) {
            return Ok(FlushPolicy::manual());
        }
        let mut policy = FlushPolicy::default();
        for field in spec.split(',') {
            let field = field.trim();
            if field.is_empty() {
                continue; // tolerate a trailing comma
            }
            let (key, value) = field.split_once('=').ok_or_else(|| {
                Error::config_msg(format!(
                    "flush policy: expected `key=value` in `{field}` (or the single word `manual`)"
                ))
            })?;
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "interval" | "every" => policy = policy.every(parse_duration(value)?),
                "buffer" | "bytes" => {
                    policy = match value.to_ascii_lowercase().as_str() {
                        "budget" | "default" => policy.size(FlushSize::Budget),
                        "off" | "none" => policy.size(FlushSize::Off),
                        _ => policy
                            .at_buffer_bytes(parse_bytes(value)?.min(usize::MAX as u64) as usize),
                    }
                }
                "rows" => policy = policy.at_buffer_rows(parse_count(value)?),
                "wal" => policy = policy.at_wal_bytes(parse_bytes(value)?),
                "idle" => policy = policy.after_idle(parse_duration(value)?),
                "tick" => policy = policy.tick(parse_duration(value)?),
                other => {
                    return Err(Error::config_msg(format!(
                        "flush policy: unknown key `{other}` (expected interval/buffer/rows/wal/idle/tick)"
                    )));
                }
            }
        }
        Ok(policy)
    }
}

/// Renders the spec syntax [`FromStr`] accepts, so a host can log the effective policy.
impl fmt::Display for FlushPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_manual() {
            return f.write_str("manual");
        }
        let mut sep = "";
        let mut field = |f: &mut fmt::Formatter<'_>, text: String| -> fmt::Result {
            f.write_str(sep)?;
            sep = ",";
            f.write_str(&text)
        };
        if let Some(d) = self.interval {
            field(f, format!("interval={}", DurationSpec(d)))?;
        }
        match self.size {
            FlushSize::Budget => field(f, "buffer=budget".to_owned())?,
            FlushSize::Bytes(n) => field(f, format!("buffer={n}"))?,
            FlushSize::Off => field(f, "buffer=off".to_owned())?,
        }
        if let Some(n) = self.rows {
            field(f, format!("rows={n}"))?;
        }
        if let Some(n) = self.wal_bytes {
            field(f, format!("wal={n}"))?;
        }
        if let Some(d) = self.idle {
            field(f, format!("idle={}", DurationSpec(d)))?;
        }
        field(f, format!("tick={}", DurationSpec(self.tick)))
    }
}

/// A [`Duration`] in the spec's own syntax (`500ms` / `5s` / `2m`), for [`FlushPolicy`]'s `Display`.
struct DurationSpec(Duration);

impl fmt::Display for DurationSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = self.0.as_millis();
        // Prefer the coarsest unit that stays exact, so a round-trip through `FromStr` is lossless
        // and the rendering reads the way it was written.
        if ms == 0 {
            f.write_str("0s")
        } else if ms.is_multiple_of(3_600_000) {
            write!(f, "{}h", ms / 3_600_000)
        } else if ms.is_multiple_of(60_000) {
            write!(f, "{}m", ms / 60_000)
        } else if ms.is_multiple_of(1_000) {
            write!(f, "{}s", ms / 1_000)
        } else {
            write!(f, "{ms}ms")
        }
    }
}

/// Parse a duration in the config spec's syntax: an integer with an optional `ms` / `s` / `m` / `h`
/// suffix (bare numbers are seconds). Fractions are deliberately not accepted — `500ms` is
/// unambiguous where `0.5` is not.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (digits, unit) = split_unit(s);
    let n: u64 = digits.parse().map_err(|_| {
        Error::config_msg(format!(
            "`{s}` is not a duration (expected e.g. 500ms, 5s, 2m, 1h)"
        ))
    })?;
    match unit.to_ascii_lowercase().as_str() {
        "" | "s" | "sec" | "secs" => Ok(Duration::from_secs(n)),
        "ms" => Ok(Duration::from_millis(n)),
        "m" | "min" | "mins" => Ok(Duration::from_secs(n * 60)),
        "h" | "hr" | "hrs" => Ok(Duration::from_secs(n * 3600)),
        other => Err(Error::config_msg(format!(
            "`{s}` has an unknown duration unit `{other}` (expected ms, s, m or h)"
        ))),
    }
}

/// Parse a byte size: an integer with an optional binary (`KiB`/`MiB`/`GiB`) or decimal
/// (`KB`/`MB`/`GB`) suffix; a bare number is bytes. `K`/`M`/`G` alone are binary, matching how
/// operators usually mean them.
pub fn parse_bytes(s: &str) -> Result<u64> {
    let s = s.trim();
    let (digits, unit) = split_unit(s);
    let n: u64 = digits.parse().map_err(|_| {
        Error::config_msg(format!(
            "`{s}` is not a byte size (expected e.g. 8388608, 16MiB, 1GB)"
        ))
    })?;
    let scale: u64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kib" => 1 << 10,
        "m" | "mib" => 1 << 20,
        "g" | "gib" => 1 << 30,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        other => {
            return Err(Error::config_msg(format!(
                "`{s}` has an unknown size unit `{other}` (expected KiB, MiB, GiB, KB, MB or GB)"
            )));
        }
    };
    n.checked_mul(scale)
        .ok_or_else(|| Error::config_msg(format!("`{s}` overflows a byte count")))
}

/// Parse a plain count (`rows=`), allowing `_` separators for readability.
fn parse_count(s: &str) -> Result<u64> {
    let cleaned: String = s.chars().filter(|c| *c != '_').collect();
    cleaned
        .trim()
        .parse()
        .map_err(|_| Error::config_msg(format!("`{s}` is not a row count")))
}

/// Split a config scalar into its leading digits and its unit suffix. Underscores in the number are
/// dropped so `16_384` parses.
fn split_unit(s: &str) -> (String, &str) {
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '_')
        .unwrap_or(s.len());
    let digits = s[..split].chars().filter(|c| *c != '_').collect();
    (digits, s[split..].trim())
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

/// What to do about two metric datapoints that share a series **and** a timestamp (ARCHITECTURE.md
/// §10.5, issue #27).
///
/// PromQL's series identity is `service` + `__name__` + the string attributes, so two points sharing
/// all of those *and* an instant are ambiguous to every metrics reader — there is no PromQL meaning
/// for two values at one timestamp. This knob picks which end of the pipeline says so.
///
/// [`Duplicates::ErrorOnRead`] (the default) keeps the historical behavior: ingest takes everything
/// and a PromQL query over an affected series fails. [`Duplicates::LastWins`] instead resolves the
/// ambiguity at read time, so one bad point degrades one instant rather than the whole metric — the
/// escape hatch for a database that *already* holds duplicates, since no ingest-side policy can
/// repair data that is already written. [`Duplicates::Reject`] catches the repeat at ingest, where
/// the responsible producer can see it in `IngestReceipt::rejected`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Duplicates {
    /// Accept every point at ingest; fail a PromQL query that materializes a series holding two
    /// samples at one timestamp, naming the metric, the label set and the instant. The default, and
    /// byte-for-byte the behavior imbh has always had.
    #[default]
    ErrorOnRead,
    /// Accept every point at ingest; collapse a duplicated timestamp to a single point at read time
    /// instead of failing the query.
    ///
    /// The surviving point is chosen by a total order on the *value*, never by scan order: metric
    /// segments carry no ingest-sequence column and the read SQL orders by time alone, so "the last
    /// one the scan emitted" would let two identical queries disagree after a flush or a compaction.
    /// The collapse is therefore a pure function of the fetched samples.
    LastWins,
    /// Drop a metric point whose `(series, timestamp)` is already among the last `recent` accepted
    /// points, and count it in `IngestReceipt::rejected`. Reads stay as strict as
    /// [`Duplicates::ErrorOnRead`], since points written before this policy was enabled are still
    /// there.
    ///
    /// `recent` bounds the guard's memory *and* its lookback: roughly `recent / points_per_second` of
    /// history at ~25-50 bytes per remembered point. Size it at `>= 2 * peak_points_per_second *
    /// flush_interval`, so a generation rotation cannot fall inside one WAL-replay window.
    ///
    /// The guard is process-local and never persisted: it starts empty at every open and is rebuilt
    /// from the WAL tail during recovery. That is deliberate — an empty guard is strictly *more*
    /// permissive, which is what makes replay unable to drop a point the writer accepted. It follows
    /// that this is a best-effort producer-facing guard, not a storage-level uniqueness constraint.
    /// In particular it does **not** reject out-of-order or late-arriving points (only an exact
    /// `(series, timestamp)` repeat), does not see duplicates older than `recent` points, and always
    /// accepts the first point per series after an open.
    Reject {
        /// How many recently accepted points to remember. Clamped to at least 2 so a generation
        /// rotation is always possible.
        recent: usize,
    },
}

impl Duplicates {
    /// The default lookback for [`Duplicates::Reject`]: 262 144 points, ~13 MB of fixed memory, and
    /// ~26 s of history at 10 000 points/s (~4.4 min at 1 000/s).
    pub const DEFAULT_RECENT: usize = 1 << 18;

    /// [`Duplicates::Reject`] with [`Self::DEFAULT_RECENT`].
    pub fn reject() -> Self {
        Duplicates::Reject {
            recent: Self::DEFAULT_RECENT,
        }
    }

    /// The configured lookback, or `None` when duplicates are not rejected at ingest.
    pub fn recent(self) -> Option<usize> {
        match self {
            Duplicates::Reject { recent } => Some(recent.max(2)),
            _ => None,
        }
    }

    /// Whether ingest drops duplicate points.
    pub fn rejects_at_ingest(self) -> bool {
        matches!(self, Duplicates::Reject { .. })
    }

    /// Whether a read collapses a duplicated timestamp instead of failing the query.
    pub fn collapses_at_read(self) -> bool {
        matches!(self, Duplicates::LastWins)
    }
}

/// Parse a duplicate-policy spec: `error_on_read` (the default), `last_wins`, or
/// `reject[,recent=N]`.
///
/// ```
/// use imbh_core::Duplicates;
/// assert_eq!("".parse::<Duplicates>().unwrap(), Duplicates::ErrorOnRead);
/// assert_eq!("last_wins".parse::<Duplicates>().unwrap(), Duplicates::LastWins);
/// assert_eq!("reject".parse::<Duplicates>().unwrap(), Duplicates::reject());
/// assert_eq!(
///     "reject,recent=1024".parse::<Duplicates>().unwrap(),
///     Duplicates::Reject { recent: 1024 },
/// );
/// assert!("nonsense".parse::<Duplicates>().is_err());
/// ```
impl FromStr for Duplicates {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let spec = s.trim();
        if spec.is_empty() {
            return Ok(Duplicates::default());
        }
        let mut fields = spec.split(',').map(str::trim).filter(|f| !f.is_empty());
        let mode = fields.next().unwrap_or_default().to_ascii_lowercase();
        let mut policy = match mode.as_str() {
            "error_on_read" | "error" | "strict" => Duplicates::ErrorOnRead,
            "last_wins" | "last-wins" | "collapse" => Duplicates::LastWins,
            "reject" => Duplicates::reject(),
            other => {
                return Err(Error::config_msg(format!(
                    "duplicates: unknown mode `{other}` (expected error_on_read, last_wins or reject)"
                )));
            }
        };
        for field in fields {
            let (key, value) = field.split_once('=').ok_or_else(|| {
                Error::config_msg(format!("duplicates: expected `key=value` in `{field}`"))
            })?;
            match key.trim().to_ascii_lowercase().as_str() {
                "recent" => {
                    // `recent` is meaningless unless ingest is guarding; rejecting the combination
                    // beats silently ignoring a knob an operator deliberately set.
                    let Duplicates::Reject { .. } = policy else {
                        return Err(Error::config_msg(format!(
                            "duplicates: `recent` applies only to `reject`, not `{mode}`"
                        )));
                    };
                    let recent = parse_count(value.trim())?.min(usize::MAX as u64) as usize;
                    policy = Duplicates::Reject {
                        recent: recent.max(2),
                    };
                }
                other => {
                    return Err(Error::config_msg(format!(
                        "duplicates: unknown key `{other}` (expected recent)"
                    )));
                }
            }
        }
        Ok(policy)
    }
}

/// Renders the spec syntax [`FromStr`] accepts, so a host can log the effective policy.
impl fmt::Display for Duplicates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Duplicates::ErrorOnRead => f.write_str("error_on_read"),
            Duplicates::LastWins => f.write_str("last_wins"),
            Duplicates::Reject { recent } => write!(f, "reject,recent={recent}"),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn default_policy_is_budget_sized_with_no_timer() {
        let p = FlushPolicy::default();
        assert_eq!(p.size_trigger(), FlushSize::Budget);
        assert_eq!(p.interval(), None);
        assert!(!p.is_manual(), "the budget size trigger is a trigger");
        assert_eq!(p.effective_tick(), FlushPolicy::DEFAULT_TICK);
        // A host that configured maintenance but no policy keeps the historical behavior: the
        // maintenance interval becomes the periodic seal cadence.
        assert_eq!(p.or_interval(S(30)).interval(), Some(S(30)));
        // …and an explicit interval is never overwritten by it.
        assert_eq!(
            FlushPolicy::periodic(S(5)).or_interval(S(30)).interval(),
            Some(S(5))
        );
    }

    #[test]
    fn manual_disables_every_trigger() {
        let p = FlushPolicy::manual();
        assert!(p.is_manual());
        assert!(!p.triggered(1 << 30, 1_000_000, Some(1 << 30), S(60), 8 << 20));
    }

    #[test]
    fn triggers_are_or_ed_and_need_a_non_empty_buffer() {
        let threshold = 8 << 20;
        let z = Duration::ZERO;
        // Size (budget-derived).
        assert!(FlushPolicy::default().triggered(threshold, 1, None, z, threshold));
        assert!(!FlushPolicy::default().triggered(threshold - 1, 1, None, z, threshold));
        // Size (explicit) overrides the budget-derived threshold in both directions.
        let small = FlushPolicy::size_based(1024);
        assert!(small.triggered(1024, 1, None, z, threshold));
        assert!(!FlushPolicy::default().triggered(1024, 1, None, z, threshold));
        // Rows.
        let rows = FlushPolicy::manual().at_buffer_rows(10);
        assert!(rows.triggered(0, 10, None, z, threshold));
        assert!(!rows.triggered(0, 9, None, z, threshold));
        // WAL bytes — and an unmeasured WAL size never fires the trigger.
        let wal = FlushPolicy::manual().at_wal_bytes(4096);
        assert!(wal.triggered(0, 1, Some(4096), z, threshold));
        assert!(!wal.triggered(0, 1, Some(4095), z, threshold));
        assert!(!wal.triggered(0, 1, None, z, threshold));
        // Idle.
        let idle = FlushPolicy::manual().after_idle(S(2));
        assert!(idle.triggered(0, 1, None, S(2), threshold));
        assert!(!idle.triggered(0, 1, None, S(1), threshold));
        // An empty buffer never seals, whatever the triggers say — an idle policy would otherwise
        // ask for a no-op seal every tick of a quiet DB.
        assert!(!idle.triggered(0, 0, None, S(60), threshold));
        assert!(!small.triggered(1 << 30, 0, None, z, threshold));
    }

    #[test]
    fn tick_tracks_the_shortest_trigger_and_is_clamped() {
        // A sub-tick interval pulls the tick down with it, so `interval=200ms` needs no `tick=`.
        assert_eq!(
            FlushPolicy::periodic(Duration::from_millis(200)).effective_tick(),
            Duration::from_millis(200)
        );
        assert_eq!(
            FlushPolicy::default()
                .after_idle(Duration::from_millis(50))
                .effective_tick(),
            Duration::from_millis(50)
        );
        // `tick=0` cannot spin the scheduler, and an absurd tick cannot stall a close().
        assert_eq!(
            FlushPolicy::default().tick(Duration::ZERO).effective_tick(),
            FlushPolicy::MIN_TICK
        );
        assert_eq!(
            FlushPolicy::default().tick(S(86_400)).effective_tick(),
            FlushPolicy::MAX_TICK
        );
        // An explicit long tick still yields to a short interval.
        assert_eq!(
            FlushPolicy::periodic(Duration::from_millis(10))
                .tick(S(60))
                .effective_tick(),
            Duration::from_millis(10)
        );
    }

    #[test]
    fn spec_parses_every_strategy() {
        let p: FlushPolicy = "interval=5s,buffer=16MiB,rows=50_000,wal=64MiB,idle=2s,tick=250ms"
            .parse()
            .unwrap();
        assert_eq!(p.interval(), Some(S(5)));
        assert_eq!(p.size_trigger(), FlushSize::Bytes(16 << 20));
        assert_eq!(p.buffer_rows(), Some(50_000));
        assert_eq!(p.wal_bytes(), Some(64 << 20));
        assert_eq!(p.idle(), Some(S(2)));
        assert_eq!(p.effective_tick(), Duration::from_millis(250));
        assert!(p.needs_wal_bytes());

        // Aliases, whitespace, a trailing comma, and the size-trigger words.
        let p: FlushPolicy = " every = 2m , buffer = off , ".parse().unwrap();
        assert_eq!(p.interval(), Some(S(120)));
        assert_eq!(p.size_trigger(), FlushSize::Off);
        assert_eq!(
            "buffer=budget"
                .parse::<FlushPolicy>()
                .unwrap()
                .size_trigger(),
            FlushSize::Budget
        );
        // The empty spec is "unset", not an error: an unset environment variable and an empty one
        // should mean the same thing to a host.
        assert_eq!("".parse::<FlushPolicy>().unwrap(), FlushPolicy::default());
        assert_eq!("  ".parse::<FlushPolicy>().unwrap(), FlushPolicy::default());
        for word in ["manual", "MANUAL", "off", "none", "never"] {
            assert!(word.parse::<FlushPolicy>().unwrap().is_manual(), "{word}");
        }
    }

    #[test]
    fn spec_rejects_typos_rather_than_ignoring_them() {
        for bad in [
            "intervl=5s",                  // unknown key
            "interval",                    // no `=`
            "interval=5x",                 // unknown duration unit
            "interval=abc",                // not a number
            "wal=16TiB",                   // unknown size unit
            "buffer=1.5MiB",               // fractions are not accepted
            "rows=lots",                   // not a count
            "wal=99999999999999999999GiB", // overflows
        ] {
            let err = bad.parse::<FlushPolicy>().unwrap_err();
            assert!(err.is_user_error(), "{bad} -> {err}");
        }
    }

    #[test]
    fn display_round_trips_through_from_str() {
        for spec in [
            "manual",
            "interval=5s,buffer=budget,tick=1s",
            "interval=90m,buffer=1024,rows=100,wal=1000000,idle=500ms,tick=5ms",
            "buffer=off,idle=2h,tick=1s",
        ] {
            let parsed: FlushPolicy = spec.parse().unwrap();
            let rendered = parsed.to_string();
            assert_eq!(
                rendered.parse::<FlushPolicy>().unwrap(),
                parsed,
                "{spec} rendered as {rendered}"
            );
        }
        assert_eq!(FlushPolicy::manual().to_string(), "manual");
        assert_eq!(
            FlushPolicy::periodic(S(5)).to_string(),
            "interval=5s,buffer=budget,tick=1s"
        );
    }

    #[test]
    fn size_and_duration_units() {
        assert_eq!(parse_bytes("8388608").unwrap(), 8 << 20);
        assert_eq!(parse_bytes("16MiB").unwrap(), 16 << 20);
        assert_eq!(parse_bytes("16m").unwrap(), 16 << 20); // bare M means binary
        assert_eq!(parse_bytes("2GB").unwrap(), 2_000_000_000);
        assert_eq!(parse_bytes("4 KiB").unwrap(), 4096);
        assert_eq!(parse_duration("30").unwrap(), S(30)); // bare number = seconds
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("2M").unwrap(), S(120)); // units are case-insensitive
        assert_eq!(parse_duration("1h").unwrap(), S(3600));
    }

    #[test]
    fn duplicates_defaults_to_the_historical_read_time_error() {
        let d = Duplicates::default();
        assert_eq!(d, Duplicates::ErrorOnRead);
        assert!(!d.rejects_at_ingest(), "ingest rejection must be opt-in");
        assert!(!d.collapses_at_read(), "the default read stays strict");
        assert_eq!(d.recent(), None);
    }

    #[test]
    fn duplicates_spec_round_trips() {
        for spec in [
            "error_on_read",
            "last_wins",
            "reject,recent=262144",
            "reject,recent=2",
        ] {
            let parsed: Duplicates = spec.parse().unwrap();
            assert_eq!(parsed.to_string(), spec);
            assert_eq!(spec.parse::<Duplicates>().unwrap(), parsed);
        }
        assert_eq!("".parse::<Duplicates>().unwrap(), Duplicates::ErrorOnRead);
        assert_eq!(
            "  REJECT ".parse::<Duplicates>().unwrap(),
            Duplicates::reject()
        );
        assert_eq!(
            "reject,recent=262_144".parse::<Duplicates>().unwrap(),
            Duplicates::reject()
        );
    }

    #[test]
    fn duplicates_floors_recent_so_a_generation_can_always_rotate() {
        assert_eq!(
            "reject,recent=0".parse::<Duplicates>().unwrap(),
            Duplicates::Reject { recent: 2 }
        );
        assert_eq!(Duplicates::Reject { recent: 1 }.recent(), Some(2));
    }

    #[test]
    fn duplicates_rejects_malformed_specs() {
        // A typo must never leave a deployment quietly accepting the duplicates it asked to reject.
        for spec in [
            "nonsense",
            "reject,recent",
            "reject,recent=x",
            "reject,unknown=1",
            "last_wins,recent=8", // `recent` without a guard to size
        ] {
            assert!(
                spec.parse::<Duplicates>().is_err(),
                "{spec} should not parse"
            );
        }
    }
}
