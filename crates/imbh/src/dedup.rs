//! The opt-in duplicate-timestamp guard for metric ingest ([`Duplicates::Reject`], issue #27).
//!
//! Scope: a bounded, process-local set of recently accepted `(series, timestamp)` pairs. A point
//! whose pair is already in the set is dropped and counted in `IngestReceipt::rejected`; everything
//! else is accepted unchanged. Duplicates *within* one export are caught by the same pass, since the
//! guard inserts as it filters.
//!
//! **Why a set and not a per-series `last_timestamp`.** A `ts <= last_ts` rule is order-sensitive:
//! records X(ts=100) then Y(ts=50) accept one point, while Y then X accept two. Ingest decisions do
//! not always happen in LSN order (an async producer can be parked awaiting a queue slot while
//! another enqueues ahead of it), and WAL replay re-derives the unsealed tail from the raw bodies
//! with a guard that starts empty — so a `last_ts` rule could reject on replay a point the writer had
//! accepted, which is data loss. The set rule is order-commutative, which makes the direction
//! provable (see [`DedupGuard::retain`]) and lets the guard live above `Storage`, at the decode site,
//! where the *async* path can still report an exact rejection count. It also leaves genuinely
//! out-of-order and late-arriving data accepted, as the storage engine has always allowed
//! (ARCHITECTURE.md §7).
//!
//! Footprint: nothing is allocated unless the policy is [`Duplicates::Reject`], and the two
//! generations are preallocated so they never rehash. Only `std` is used — a producer-only build
//! (`--no-default-features --features ingest`) has no DataFusion, so nothing here may reach for the
//! query engine's `ahash` or read sealed segments.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use imbh_core::{Duplicates, ExpHistogramRow, HistogramRow, ScalarMetricRow, SummaryRow, Table};

use crate::DecodedMetrics;

/// A 128-bit series identity plus the point's event timestamp.
///
/// `[u64; 2]` rather than `u128` deliberately: `u128` has 16-byte alignment, which would pad this
/// struct from 24 to 32 bytes and cost a third more memory per remembered point.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SeenKey {
    series: [u64; 2],
    ts: i64,
}

// Domain separators, so a gauge and a histogram sharing a name and an instant are never one series.
const KIND_GAUGE: u8 = 0;
const KIND_SUM: u8 = 1;
const KIND_HISTOGRAM: u8 = 2;
const KIND_EXP_HISTOGRAM: u8 = 3;
const KIND_SUMMARY: u8 = 4;

/// The identity fields every metric row kind shares, as the guard reads them.
trait SeriesIdentity {
    fn kind(&self) -> u8;
    fn metric(&self) -> &str;
    fn service(&self) -> Option<&str>;
    fn attributes(&self) -> &str;
    fn ts(&self) -> i64;

    fn key(&self) -> SeenKey {
        SeenKey {
            series: series_hash(
                self.kind(),
                self.metric(),
                self.service(),
                self.attributes(),
            ),
            ts: self.ts(),
        }
    }
}

impl SeriesIdentity for ScalarMetricRow {
    fn kind(&self) -> u8 {
        // Separate discriminants on purpose. An instant selector reads the gauge *and* the sum
        // table and unions them, so a name emitted as both kinds does merge into one PromQL series
        // — but a false rejection here is silent data loss, so a legitimate sum point must never be
        // dropped because a same-named gauge existed. The read side resolves the merged case.
        if self.table == Table::MetricsSum {
            KIND_SUM
        } else {
            KIND_GAUGE
        }
    }
    fn metric(&self) -> &str {
        &self.metric
    }
    fn service(&self) -> Option<&str> {
        self.service.as_deref()
    }
    fn attributes(&self) -> &str {
        &self.attributes
    }
    fn ts(&self) -> i64 {
        self.time_unix_nano
    }
}

macro_rules! identity {
    ($row:ty, $kind:expr) => {
        impl SeriesIdentity for $row {
            fn kind(&self) -> u8 {
                $kind
            }
            fn metric(&self) -> &str {
                &self.metric
            }
            fn service(&self) -> Option<&str> {
                self.service.as_deref()
            }
            fn attributes(&self) -> &str {
                &self.attributes
            }
            fn ts(&self) -> i64 {
                self.time_unix_nano
            }
        }
    };
}

identity!(HistogramRow, KIND_HISTOGRAM);
identity!(ExpHistogramRow, KIND_EXP_HISTOGRAM);
identity!(SummaryRow, KIND_SUMMARY);

/// Hash the fields PromQL turns into a label set: `__name__` (the metric), `service`, and the
/// attributes.
///
/// `attributes` is `canonical_json_object` output with sorted keys, so byte-equality of that string
/// *is* label-set equality — no parsing needed. `unit`, `flags`, `start_time_unix_nano`,
/// `temporality`, `is_monotonic`, `resource` and `scope` are excluded because PromQL ignores all of
/// them: including any would let a duplicate slip through by varying a field no reader can see.
///
/// 128 bits, from two salted passes, because a collision here is a *false rejection*, i.e. silent
/// data loss. Over a million series the birthday bound is ~2.7e-8 at 64 bits against ~1.5e-27 at 128.
/// `DefaultHasher` rather than the query engine's `ahash`: a producer-only build must gain no
/// dependency.
fn series_hash(kind: u8, metric: &str, service: Option<&str>, attributes: &str) -> [u64; 2] {
    const SALTS: [u64; 2] = [0x9e37_79b9_7f4a_7c15, 0xc2b2_ae3d_27d4_eb4f];
    let mut out = [0u64; 2];
    for (slot, salt) in out.iter_mut().zip(SALTS) {
        let mut h = DefaultHasher::new();
        salt.hash(&mut h);
        kind.hash(&mut h);
        // `str: Hash` length-prefixes via a 0xff terminator byte, so ("ab", "c") cannot alias
        // ("a", "bc"); `Option<&str>` writes its own discriminant, keeping `None` distinct from
        // `Some("")`.
        metric.hash(&mut h);
        service.hash(&mut h);
        attributes.hash(&mut h);
        *slot = h.finish();
    }
    out
}

/// Two generations of remembered keys. Lookups consult both; inserts land in `current`. When
/// `current` fills, it becomes `previous` and the old `previous` is dropped whole — O(1) amortized
/// eviction with no per-entry bookkeeping, and live entries always number between `cap` and `2*cap`.
struct GuardState {
    current: HashSet<SeenKey>,
    previous: HashSet<SeenKey>,
    /// Rotate once `current` reaches this: half of the configured `recent`.
    cap: usize,
}

impl GuardState {
    fn new(recent: usize) -> Self {
        let cap = (recent / 2).max(1);
        GuardState {
            // Preallocated so neither generation ever rehashes: the guard's memory is fixed for the
            // process lifetime instead of doubling under load.
            current: HashSet::with_capacity(cap),
            previous: HashSet::with_capacity(cap),
            cap,
        }
    }

    /// `true` when the key is new (and it is then remembered); `false` when it is a duplicate.
    fn admit(&mut self, key: SeenKey) -> bool {
        if self.current.contains(&key) || self.previous.contains(&key) {
            return false;
        }
        if self.current.len() >= self.cap {
            let retired = std::mem::replace(&mut self.current, HashSet::with_capacity(self.cap));
            self.previous = retired;
        }
        self.current.insert(key);
        true
    }

    #[cfg(test)]
    fn remembered(&self) -> usize {
        self.current.len() + self.previous.len()
    }
}

/// The per-database duplicate guard. Cheap and inert unless the policy is [`Duplicates::Reject`].
pub(crate) struct DedupGuard {
    /// `None` unless rejecting — no allocation, no lock, nothing on the hot path.
    state: Option<Mutex<GuardState>>,
    rejected: AtomicU64,
}

impl DedupGuard {
    pub(crate) fn new(policy: Duplicates) -> Self {
        DedupGuard {
            state: policy.recent().map(|n| Mutex::new(GuardState::new(n))),
            rejected: AtomicU64::new(0),
        }
    }

    /// Points rejected since open, for `stats().ingest_rejected`.
    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    /// Drop every point whose `(series, timestamp)` the guard already knows, remembering the
    /// survivors. Returns how many were dropped.
    ///
    /// The lock is taken **once per export**, not once per point, and `Vec::retain` preserves order,
    /// so the surviving rows keep the order the decoder produced.
    ///
    /// The replay direction, which is what makes this safe: the guard starts empty at every open, so
    /// the replay guard's key set is always a subset of the writer's at the same record. A point the
    /// writer accepted was therefore absent from the writer's set, hence absent from replay's, hence
    /// accepted again. **Replay is strictly more permissive and can never drop a row the writer
    /// kept.** The residual drifts the other way: a duplicate whose predecessor was already sealed
    /// is re-accepted after a restart, which the read side then resolves.
    pub(crate) fn retain(&self, decoded: &mut DecodedMetrics) -> u64 {
        let Some(state) = self.state.as_ref() else {
            return 0;
        };
        // One exemplar per export is enough to identify the producer, and it keeps the log from
        // scaling with a duplicating producer's point rate.
        let mut exemplar: Option<String> = None;
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let dropped = filter(&mut decoded.rows, &mut state, &mut exemplar)
            + filter(&mut decoded.histograms, &mut state, &mut exemplar)
            + filter(&mut decoded.exp_histograms, &mut state, &mut exemplar)
            + filter(&mut decoded.summaries, &mut state, &mut exemplar);
        drop(state);
        if dropped > 0 {
            self.rejected.fetch_add(dropped, Ordering::Relaxed);
            #[cfg(feature = "tracing")]
            tracing::warn!(
                rejected = dropped,
                exemplar = exemplar.as_deref().unwrap_or("?"),
                "dropped duplicate-timestamp metric points (Duplicates::Reject)"
            );
        }
        dropped
    }

    #[cfg(test)]
    fn remembered(&self) -> usize {
        self.state
            .as_ref()
            .map_or(0, |s| s.lock().unwrap().remembered())
    }
}

/// Drop the rows the guard already knows, filling `exemplar` from the first one dropped (if it is
/// still empty) so the caller can name the offending series in one log line.
fn filter<R: SeriesIdentity>(
    rows: &mut Vec<R>,
    state: &mut GuardState,
    exemplar: &mut Option<String>,
) -> u64 {
    let before = rows.len();
    rows.retain(|row| {
        let admitted = state.admit(row.key());
        if !admitted && exemplar.is_none() {
            *exemplar = Some(format!(
                "{}{{service={}}}{} at {}",
                row.metric(),
                row.service().unwrap_or("-"),
                row.attributes(),
                row.ts()
            ));
        }
        admitted
    });
    (before - rows.len()) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauge(metric: &str, attributes: &str, ts: i64) -> ScalarMetricRow {
        ScalarMetricRow {
            table: Table::MetricsGauge,
            time_unix_nano: ts,
            start_time_unix_nano: None,
            metric: metric.to_owned(),
            unit: String::new(),
            service: Some("cart".to_owned()),
            attributes: attributes.to_owned(),
            resource: "{}".to_owned(),
            scope: "{}".to_owned(),
            flags: 0,
            value: 1.0,
            temporality: None,
            is_monotonic: None,
            exemplars: "[]".to_owned(),
        }
    }

    fn decoded(rows: Vec<ScalarMetricRow>) -> DecodedMetrics {
        DecodedMetrics {
            rows,
            histograms: Vec::new(),
            exp_histograms: Vec::new(),
            summaries: Vec::new(),
            accepted: 0,
            rejected: 0,
        }
    }

    #[test]
    fn accept_policy_allocates_nothing_and_drops_nothing() {
        let guard = DedupGuard::new(Duplicates::ErrorOnRead);
        assert!(guard.state.is_none(), "the default must not allocate");
        let mut d = decoded(vec![gauge("m", "{}", 10), gauge("m", "{}", 10)]);
        assert_eq!(guard.retain(&mut d), 0);
        assert_eq!(d.rows.len(), 2);

        // LastWins is a read-side resolution; ingest still takes everything.
        let guard = DedupGuard::new(Duplicates::LastWins);
        assert!(guard.state.is_none());
        let mut d = decoded(vec![gauge("m", "{}", 10), gauge("m", "{}", 10)]);
        assert_eq!(guard.retain(&mut d), 0);
        assert_eq!(d.rows.len(), 2);
    }

    #[test]
    fn reject_drops_the_repeat_within_and_across_exports() {
        let guard = DedupGuard::new(Duplicates::reject());
        // Within one export.
        let mut d = decoded(vec![gauge("m", "{}", 10), gauge("m", "{}", 10)]);
        assert_eq!(guard.retain(&mut d), 1);
        assert_eq!(d.rows.len(), 1);
        // Across exports — the case issue #27 actually hit.
        let mut d = decoded(vec![gauge("m", "{}", 10)]);
        assert_eq!(guard.retain(&mut d), 1);
        assert!(d.rows.is_empty());
        assert_eq!(guard.rejected(), 2);
    }

    #[test]
    fn reject_keeps_distinct_timestamps_labels_and_kinds() {
        let guard = DedupGuard::new(Duplicates::reject());
        let mut sum = gauge("m", "{}", 10);
        sum.table = Table::MetricsSum;
        let mut other_service = gauge("m", "{}", 10);
        other_service.service = Some("checkout".to_owned());
        let mut no_service = gauge("m", "{}", 10);
        no_service.service = None;
        let mut d = decoded(vec![
            gauge("m", "{}", 10),
            gauge("m", "{}", 11),              // distinct instant
            gauge("m", "{\"pod\":\"a\"}", 10), // distinct labels
            gauge("other", "{}", 10),          // distinct metric
            sum,                               // distinct instrument kind
            other_service,                     // distinct service
            no_service,                        // `None` is not `Some("")`
        ]);
        assert_eq!(guard.retain(&mut d), 0);
        assert_eq!(d.rows.len(), 7);
    }

    #[test]
    fn reject_accepts_out_of_order_points() {
        // Only an exact (series, timestamp) repeat is a duplicate. Backfill and multi-producer clock
        // skew must keep working — this is the boundary against a `last_timestamp` rule.
        let guard = DedupGuard::new(Duplicates::reject());
        let mut d = decoded(vec![gauge("m", "{}", 200)]);
        assert_eq!(guard.retain(&mut d), 0);
        let mut d = decoded(vec![gauge("m", "{}", 100)]);
        assert_eq!(guard.retain(&mut d), 0);
        assert_eq!(d.rows.len(), 1);
    }

    #[test]
    fn the_guard_is_bounded_and_eventually_forgets() {
        let guard = DedupGuard::new(Duplicates::Reject { recent: 8 });
        for ts in 0..12 {
            let mut d = decoded(vec![gauge("m", "{}", ts)]);
            assert_eq!(guard.retain(&mut d), 0);
            assert!(guard.remembered() <= 8, "bound exceeded at ts={ts}");
        }
        // The oldest generation has been dropped, so the first point is new again. Re-admitting an
        // evicted key is the guard failing *permissive*, which is the safe direction.
        let mut d = decoded(vec![gauge("m", "{}", 0)]);
        assert_eq!(guard.retain(&mut d), 0);
    }

    #[test]
    fn replay_never_rejects_what_a_fuller_guard_accepted() {
        // The invariant `G_replay ⊆ G_original`, exercised directly: whatever a warm guard accepts,
        // a guard that started empty on the same suffix accepts too.
        let warm = DedupGuard::new(Duplicates::reject());
        let mut seed = decoded(vec![gauge("m", "{}", 1), gauge("m", "{}", 2)]);
        warm.retain(&mut seed);

        let suffix = || {
            vec![
                gauge("m", "{}", 2),
                gauge("m", "{}", 3),
                gauge("m", "{}", 3),
            ]
        };
        let mut original = decoded(suffix());
        warm.retain(&mut original);

        let cold = DedupGuard::new(Duplicates::reject());
        let mut replayed = decoded(suffix());
        cold.retain(&mut replayed);

        assert!(
            replayed.rows.len() >= original.rows.len(),
            "replay accepted {} rows, fewer than the writer's {}",
            replayed.rows.len(),
            original.rows.len()
        );
    }

    #[test]
    fn series_hash_separates_every_identity_field() {
        let base = series_hash(KIND_GAUGE, "m", Some("cart"), "{}");
        for other in [
            series_hash(KIND_SUM, "m", Some("cart"), "{}"),
            series_hash(KIND_GAUGE, "n", Some("cart"), "{}"),
            series_hash(KIND_GAUGE, "m", Some("checkout"), "{}"),
            series_hash(KIND_GAUGE, "m", None, "{}"),
            series_hash(KIND_GAUGE, "m", Some(""), "{}"),
            series_hash(KIND_GAUGE, "m", Some("cart"), "{\"pod\":\"a\"}"),
            // Field boundaries must not smear into one another.
            series_hash(KIND_GAUGE, "mcart", Some(""), "{}"),
        ] {
            assert_ne!(base, other);
        }
        assert_eq!(base, series_hash(KIND_GAUGE, "m", Some("cart"), "{}"));
    }
}
