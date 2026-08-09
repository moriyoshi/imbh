//! Bounded-memory accumulators and the sigma summary.
//!
//! The measurement is a two-level count. For one scan unit (a table, or the whole DB) we hold a map
//! `key -> (row counters, value map)`, and the value map holds `value -> #segments containing it`.
//! Sigma for a `(key, value)` pair is `#segments containing it / #segments scanned`, so a
//! segment-granularity index prunes `1 - sigma` of the segments for a `key = value` predicate.
//!
//! Both levels are [`SampledMap`]s, which is what keeps memory bounded: an attribute key can be
//! unique per row (`trace_id`-shaped keys, or a producer that folds an id into the key), so a naive
//! "collect every distinct pair" is unbounded in both dimensions. Entry *size* is bounded too: a
//! sketch keys by the hash of a name rather than storing it, so the worst case really is
//! `cap x constant`, not `cap x whatever the longest attribute value happened to be`.
//!
//! ## Cardinality is a curve, not a number
//!
//! A single global distinct-value count cannot tell apart the two shapes that behave completely
//! differently at query time:
//!
//! - **interleaved** — every segment carries every value (`env`, `service`). Global cardinality can
//!   be large and *nothing* prunes: every segment matches.
//! - **localized** — a value lives in a few consecutive segments and never recurs (`pod.name` across
//!   a rolling deploy, a session id). Global cardinality is large *because* values churn, and
//!   segment-granularity pruning removes almost everything.
//!
//! Sigma already distinguishes them, but only at one scale — the segment. The same key can be
//! localized against a day and interleaved against a minute, and which one matters depends on the
//! range the user queries over. So the accumulator counts distinct values at a **ladder of window
//! widths**: the segment (innermost, = sigma), a few wall-clock widths, and the
//! whole scan (outermost, = global cardinality). `C(w)`, the mean distinct values within one window
//! of width `w`, is then a curve, and its *shape* is the answer:
//!
//! - flat (`C(seg) ~ C(all)`) — interleaved. No pruning mechanism helps; a promoted column is the
//!   only lever.
//! - rising — localized. The width at which it flattens is the horizon beyond which segment pruning
//!   stops paying, which is exactly the input the reactive cost gate in `imbh-query` lacks.
//!
//! Counting distinct *windows* per value reuses the same "last ordinal seen" trick as the segment
//! count, so it needs no extra passes — one `u32` pair per level per tracked value. It does require
//! that segments arrive in **nondecreasing time order** — see [`Acc::begin_segment`].
//!
//! The per-value ladder state is the one thing here that scales with the value cap: two `Vec`s per
//! tracked value, so `--max-values` governs it and `--windows none` removes it entirely.

use std::collections::BTreeMap;

use imbh_core::{AnyValue, canonical_json_value};

/// Sentinel "not yet seen in any segment" ordinal.
const NO_SEG: u32 = u32::MAX;

fn hash(key: &str) -> u64 {
    xxhash_rust::xxh3::xxh3_64(key.as_bytes())
}

/// Which attribute column a key came from. Promotion (`DbBuilder::promote`) applies to
/// [`AttrScope::Attributes`] **only** — `lookup_promoted` in `imbh-storage` is
/// `json_get(attributes, key)` keeping `AnyValue::Str`, and deliberately does not merge
/// `resource`/`scope` (they are different scopes). A segment index has no such restriction, so the
/// sigma side reports all three, prefixed so they never collide.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum AttrScope {
    Attributes,
    Resource,
    Scope,
}

impl AttrScope {
    pub fn column(self) -> &'static str {
        match self {
            AttrScope::Attributes => "attributes",
            AttrScope::Resource => "resource",
            AttrScope::Scope => "scope",
        }
    }

    /// Prefix applied to the reported key name. Record attributes keep their bare name so the
    /// promotion report's keys read exactly as they would in a `promote = [...]` list.
    pub fn prefix(self) -> &'static str {
        match self {
            AttrScope::Attributes => "",
            AttrScope::Resource => "resource:",
            AttrScope::Scope => "scope:",
        }
    }
}

/// A map bounded by a **bottom-k sketch**, keyed by the hash of the entry's name.
///
/// It retains the `cap` entries whose names hash smallest — a deterministic, uniform sample of the
/// population, since the hash is uniform and the retained set is a pure function of the *set* of
/// names seen. `bound` is the largest hash still admissible once anything has been dropped; while
/// nothing has, the map holds the entire population and every count is exact.
///
/// **A `BTreeMap` keyed by hash is both the lookup structure and the order structure.** The only
/// ordering operation this needs is "remove the largest", which `last_key_value`/`pop_last` give
/// directly. An earlier version kept a `HashMap<Rc<str>, V>` beside a `BTreeSet<(u64, Rc<str>)>`;
/// that set was never sparse — it held an entry for *every* key, always — so it was a full shadow
/// index of the same population, costing 24 bytes plus a node per entry to answer one question. It
/// also meant hashing every name twice on the hot path, once with SipHash for the `HashMap` probe
/// and once with xxh3 for the sampling predicate.
///
/// Storing the *name* is the caller's business, and only one caller needs to: [`KeyAcc`] carries a
/// `name` because the report prints attribute keys, while [`ValueAcc`] carries none because a value's
/// text is never read back — it exists only to tell values apart, which the hash already does. That
/// is what removes the value text from memory entirely, and with it the digest-folding special case
/// the previous version needed to bound the bytes a single long attribute value could occupy.
///
/// Keying by hash means two names sharing an xxh3-64 merge into one entry. That is the collision
/// bound the digest folding already accepted, now applied uniformly: at the default 50,000-value cap
/// the probability is about 7e-11, and at the 4,096-key cap about 5e-13.
///
/// Three properties hold, and all three are pinned by tests:
///
/// 1. **Counters are complete, never partial.** A name in the final bottom-`cap` was in the
///    bottom-`cap` of every prefix that contained it (a subset's bottom-`cap` cannot exclude a
///    member of the whole set's), so it was admitted on first sight and never evicted — eviction
///    only ever removes the current maximum, and a name in the final sample is never the maximum of
///    a full map.
/// 2. **The result does not depend on arrival order**, because "the `cap` smallest hashes" is a
///    property of the name set alone.
/// 3. **Folding is exact.** Merging two sketches and re-truncating yields precisely what one pass
///    over the union would: the bottom-`cap` of a union is the bottom-`cap` of the parts' bottoms.
///    That is what makes a per-segment sketch a viable basis for the cardinality ladder.
///
/// This replaced an adaptive halving scheme (`shift`, `hash(k) <= u64::MAX >> shift`) which had
/// none of properties 2 and 3: it halved whenever the map was full *at the moment a key arrived*,
/// so different arrival orders reached different rates, kept different keys, and reported different
/// `estimated_total`s over identical input. An exhaustive permutation search found tens of
/// thousands of disagreeing pairs. Sampling *membership* was order-independent there; the *rate*
/// was not, and the rate is what reaches the report.
///
/// A hard "stop inserting at cap" cutoff is still the wrong answer, for the original reason: it
/// keeps exactly the values seen *earliest*, biasing the sigma distribution toward whatever the
/// first segments happened to contain. Bottom-k keeps a hash-uniform sample instead.
pub struct SampledMap<V> {
    entries: BTreeMap<u64, V>,
    cap: usize,
    /// Whether any entry has ever been dropped — evicted to make room, or refused on arrival. While
    /// false the sample *is* the population and `estimated_total` is a count, not an estimate.
    dropped: bool,
    /// Largest hash still admissible. Meaningful only once `dropped`; carried explicitly rather
    /// than read off `entries` so that a merge can inherit the tighter of its two inputs' bounds.
    bound: u64,
}

impl<V> SampledMap<V> {
    pub fn new(cap: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            cap: cap.max(1),
            dropped: false,
            bound: u64::MAX,
        }
    }

    fn max_hash(&self) -> u64 {
        self.entries
            .last_key_value()
            .map(|(h, _)| *h)
            .unwrap_or(u64::MAX)
    }

    /// Record that an entry was dropped and tighten the admission bound to what is actually retained.
    fn note_drop(&mut self) {
        self.dropped = true;
        self.bound = self.bound.min(self.max_hash());
    }

    /// The entry for `name`, creating it with `make` when absent. `None` means the name is not in
    /// the sample — the caller must drop the observation entirely rather than count it elsewhere.
    pub fn entry(&mut self, name: &str, make: impl FnOnce() -> V) -> Option<&mut V> {
        let h = hash(name);
        if self.entries.contains_key(&h) {
            return self.entries.get_mut(&h);
        }
        if self.dropped && h > self.bound {
            return None;
        }
        let mut evicted = false;
        if self.entries.len() >= self.cap {
            if h >= self.max_hash() {
                // Worse than everything retained: refuse it, and record that nothing above the
                // current maximum can be admitted from here on.
                self.note_drop();
                return None;
            }
            self.entries.pop_last();
            evicted = true;
        }
        self.entries.insert(h, make());
        if evicted {
            // Making room discarded another entry, so this is a sample from here on and the bound
            // tightens to what actually survived the insert.
            self.note_drop();
        }
        self.entries.get_mut(&h)
    }

    pub fn tracked(&self) -> usize {
        self.entries.len()
    }

    /// Entries with the hash they are keyed by. The hash is an identity, not a name — nothing in
    /// the report prints it; it exists so tests can compare two sketches entry for entry.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &V)> {
        self.entries.iter().map(|(h, v)| (*h, v))
    }

    /// Fraction of the hash space the sample covers: `1.0` while nothing has been dropped, else
    /// `(bound + 1) / 2^64`. Sums over the sample scale to the population by dividing by this.
    pub fn sample_rate(&self) -> f64 {
        if !self.dropped {
            return 1.0;
        }
        (self.bound as f64 + 1.0) / 2f64.powi(64)
    }

    /// Size of the population: an exact count while nothing has been dropped, otherwise
    /// `len / sample_rate`.
    ///
    /// The textbook bottom-k estimator divides `k - 1` rather than `k` by the rate, which removes a
    /// `k/(k-1)` upward bias. This uses `k`, deliberately: every other scaled quantity in the report
    /// (`postings`, each `C(w)`) is a sum over the sample divided by the same rate, and a ratio
    /// between two of them — `C(all)/C(seg)`, the locality figure — should not carry a correction on
    /// one side only. At the default 50,000-value cap the bias is 0.002%.
    pub fn estimated_total(&self) -> f64 {
        if !self.dropped {
            return self.entries.len() as f64;
        }
        self.entries.len() as f64 / self.sample_rate()
    }

    /// Fold `other` into `self`, combining entries present in both.
    ///
    /// **Test-only on purpose.** It exists to establish that a persisted per-`(segment, key)` sketch
    /// would work: the cardinality ladder would then be a *fold* over segment sketches rather than a
    /// count stored per bucket, so retention drops a segment's statistics along with the segment and
    /// no bucket is ever written twice. Nothing in the tool calls it yet.
    ///
    /// Two inputs may have dropped entries at different bounds, and one above the tighter bound was
    /// only ever visible to one side. Trimming the union to `min(bound_a, bound_b)` before the cap
    /// restores a clean threshold sample; skipping that step would let the looser side contribute
    /// entries the other silently excluded, biasing the union upward.
    #[cfg(test)]
    pub fn merge(&mut self, other: SampledMap<V>, combine: impl Fn(&mut V, V)) {
        let (inherited, inherited_dropped) = (other.bound, other.dropped);
        for (h, v) in other.entries {
            match self.entries.get_mut(&h) {
                Some(cur) => combine(cur, v),
                None => {
                    self.entries.insert(h, v);
                }
            }
        }
        if inherited_dropped {
            self.dropped = true;
            self.bound = self.bound.min(inherited);
        }
        // Trim to the inherited bound first, then to the cap.
        while self.dropped && !self.entries.is_empty() && self.max_hash() > self.bound {
            self.entries.pop_last();
        }
        while self.entries.len() > self.cap {
            self.entries.pop_last();
            self.dropped = true;
        }
        if self.dropped {
            self.bound = self.bound.min(self.max_hash());
        }
    }
}

/// Per-value state: how many distinct segments contained it, plus the last segment ordinal that
/// counted it (so a value seen on a million rows of one segment still counts that segment once
/// without a per-segment scratch set).
///
/// `windows`/`last_window` are the same pair repeated per window level, holding **window ordinals**
/// rather than raw window ids so they fit a `u32` regardless of how far apart the timestamps are.
pub struct ValueAcc {
    pub segments: u32,
    last_seg: u32,
    /// Distinct windows containing this value, per level.
    pub windows: Vec<u32>,
    last_window: Vec<u32>,
}

/// Per-key state.
pub struct KeyAcc {
    /// The attribute key, scope-prefixed. Carried here because the report prints it and the sketch
    /// keys by hash; `ValueAcc` has no counterpart because a value's text is never read back.
    pub name: String,
    pub scope: AttrScope,
    /// Rows carrying the key at all (any value type).
    pub rows_present: u64,
    /// Rows carrying the key with an `AnyValue::Str` value — exactly the rows a promoted column
    /// would be non-NULL on (`lookup_promoted`).
    pub rows_string: u64,
    pub str_len_sum: u64,
    pub str_len_max: u32,
    pub segments_present: u32,
    last_seg: u32,
    /// Times this key's value differed from the previous row's — the **run count**. A promoted
    /// column is a dictionary plus a per-row `Int32` index array, and the index array's compressed
    /// size tracks the entropy of the value sequence, which runs measure and distinct counts do not.
    /// Two keys with identical distinct counts and postings measured 9,079 B against 64,252 B on
    /// disk purely by run structure (`archetype-bench`).
    pub runs: u64,
    pub values: SampledMap<ValueAcc>,
}

impl KeyAcc {
    fn new(name: &str, scope: AttrScope, value_cap: usize) -> Self {
        Self {
            name: name.to_owned(),
            scope,
            rows_present: 0,
            rows_string: 0,
            str_len_sum: 0,
            str_len_max: 0,
            segments_present: 0,
            last_seg: NO_SEG,
            runs: 0,
            values: SampledMap::new(value_cap),
        }
    }

    /// `(key, value, segment)` postings this key would contribute to a segment index — and, divided
    /// by the segment count, the mean distinct values per segment: `C(segment)`.
    pub fn postings(&self) -> u64 {
        self.values.iter().map(|(_, v)| u64::from(v.segments)).sum()
    }

    /// The same sum one level out: `(key, value, window)` entries at window level `level`. Divided by
    /// the number of windows at that level it is `C(w)`, the mean distinct values within one window.
    pub fn window_postings(&self, level: usize) -> u64 {
        self.values
            .iter()
            .map(|(_, v)| u64::from(v.windows[level]))
            .sum()
    }
}

/// One scan unit: a single table, or the whole database (for the promotion report, which is
/// DB-wide because `promote` is DB-wide configuration).
pub struct Acc {
    pub label: String,
    /// Segments folded in so far — the sigma denominator.
    pub segments: u32,
    pub rows: u64,
    pub keys: SampledMap<KeyAcc>,
    value_cap: usize,
    cur_seg: u32,
    /// Window widths in nanoseconds, innermost first.
    widths: Vec<i64>,
    /// Distinct windows opened so far, per level — the `C(w)` denominator.
    pub windows: Vec<u32>,
    /// Raw window id (`time / width`) of the open window, per level.
    cur_window_id: Vec<i64>,
    /// Ordinal of the open window, per level.
    cur_window: Vec<u32>,
    /// Time bounds of everything folded in, for reporting the scan's span.
    pub min_time: i64,
    pub max_time: i64,
}

impl Acc {
    pub fn new(label: impl Into<String>, key_cap: usize, value_cap: usize, widths: &[i64]) -> Self {
        Self {
            label: label.into(),
            segments: 0,
            rows: 0,
            keys: SampledMap::new(key_cap),
            value_cap,
            cur_seg: NO_SEG,
            widths: widths.to_vec(),
            windows: vec![0; widths.len()],
            cur_window_id: vec![0; widths.len()],
            cur_window: vec![NO_SEG; widths.len()],
            min_time: i64::MAX,
            max_time: i64::MIN,
        }
    }

    pub fn levels(&self) -> usize {
        self.widths.len()
    }

    /// Start a new segment. Every subsequent [`Acc::observe`] is attributed to it until the next call.
    ///
    /// **Segments must be fed in nondecreasing `min_time` order.** The window ladder dedups with a
    /// "last window ordinal seen" comparison rather than a per-window set, which is only correct
    /// while windows are visited contiguously; out-of-order feeding would reopen a closed window and
    /// count its values twice. The caller sorts (see `analyze`), and both the per-table and the
    /// DB-wide unit see a sorted subsequence of the same order.
    ///
    /// A segment is attributed wholly to the window containing its `min_time`, so one that straddles
    /// a boundary lands in the earlier window. That biases `C(w)` slightly upward for widths near the
    /// segment span — the regime where the level is degenerate anyway, since it holds ~1 segment.
    pub fn begin_segment(&mut self, min_time_unix_nano: i64, max_time_unix_nano: i64) {
        self.cur_seg = self.segments;
        self.segments += 1;
        self.min_time = self.min_time.min(min_time_unix_nano);
        self.max_time = self.max_time.max(max_time_unix_nano);
        for level in 0..self.widths.len() {
            let id = min_time_unix_nano.div_euclid(self.widths[level].max(1));
            if self.cur_window[level] == NO_SEG || id != self.cur_window_id[level] {
                self.cur_window_id[level] = id;
                self.cur_window[level] = self.windows[level];
                self.windows[level] += 1;
            }
        }
    }

    pub fn add_rows(&mut self, n: u64) {
        self.rows += n;
    }

    /// Fold in `count` rows of the current segment carrying `name = value`. `name` is already
    /// scope-prefixed; `text` is the value's canonical text form (see [`value_text`]), passed in so
    /// the caller renders it once for all sinks.
    pub fn observe(
        &mut self,
        scope: AttrScope,
        name: &str,
        value: &AnyValue,
        text: &str,
        count: u64,
        new_run: bool,
    ) {
        // Destructured rather than reached through `self`, so the `cur_window` read and the
        // `keys` mutation are disjoint field borrows. Cloning `cur_window` instead would allocate
        // once per attribute pair on the per-row hot path.
        let Self {
            keys,
            cur_window,
            cur_seg,
            value_cap,
            ..
        } = self;
        let seg = *cur_seg;
        let value_cap = *value_cap;
        let levels = cur_window.len();
        let Some(key) = keys.entry(name, || KeyAcc::new(name, scope, value_cap)) else {
            return;
        };
        key.rows_present += count;
        if new_run {
            key.runs += 1;
        }
        if key.last_seg != seg {
            key.last_seg = seg;
            key.segments_present += 1;
        }
        if let AnyValue::Str(s) = value {
            let len = s.len() as u32;
            key.rows_string += count;
            key.str_len_sum += u64::from(len) * count;
            key.str_len_max = key.str_len_max.max(len);
        }
        if let Some(v) = key.values.entry(text, || ValueAcc {
            segments: 0,
            last_seg: NO_SEG,
            windows: vec![0; levels],
            last_window: vec![NO_SEG; levels],
        }) {
            if v.last_seg != seg {
                v.last_seg = seg;
                v.segments += 1;
            }
            for (level, &open) in cur_window.iter().enumerate() {
                if v.last_window[level] != open {
                    v.last_window[level] = open;
                    v.windows[level] += 1;
                }
            }
        }
    }
}

/// The canonical text form of an attribute value — the identity a segment index would key on.
/// Strings are borrowed verbatim; everything else is rendered as canonical JSON, so `5` and `"5"`
/// stay distinct values.
pub fn value_text(v: &AnyValue) -> std::borrow::Cow<'_, str> {
    match v {
        AnyValue::Str(s) => std::borrow::Cow::Borrowed(s),
        other => std::borrow::Cow::Owned(canonical_json_value(other)),
    }
}

/// The sigma distribution for one key: one sample per **distinct value**, unweighted by how often
/// that value occurs. That is the right unit for "if I filter on an arbitrary value of this key,
/// what fraction of segments must I read"; weighting by query frequency would need a query log.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SigmaSummary {
    pub p50: f64,
    pub p90: f64,
    pub max: f64,
    pub mean: f64,
    /// Fraction of values whose sigma is at or below 0.25 — the "long tail a segment index
    /// exploits" share.
    pub frac_le_25: f64,
    /// Counts in ten equal-width sigma buckets `[0,0.1) .. [0.9,1.0]`.
    pub histogram: [u32; 10],
    pub count: usize,
}

/// Summarize one key's per-value segment counts against a `segments`-segment denominator.
pub fn summarize(values: impl Iterator<Item = u32>, segments: u32) -> Option<SigmaSummary> {
    if segments == 0 {
        return None;
    }
    let denom = f64::from(segments);
    let mut sigmas: Vec<f64> = values.map(|s| f64::from(s) / denom).collect();
    if sigmas.is_empty() {
        return None;
    }
    sigmas.sort_by(|a, b| a.partial_cmp(b).expect("sigma is never NaN"));
    let n = sigmas.len();
    let pick = |q: f64| sigmas[(((n - 1) as f64) * q).round() as usize];
    let mut histogram = [0u32; 10];
    for &s in &sigmas {
        let bucket = ((s * 10.0) as usize).min(9);
        histogram[bucket] += 1;
    }
    let le_25 = sigmas.iter().filter(|&&s| s <= 0.25).count();
    Some(SigmaSummary {
        p50: pick(0.5),
        p90: pick(0.9),
        max: sigmas[n - 1],
        mean: sigmas.iter().sum::<f64>() / n as f64,
        frac_le_25: le_25 as f64 / n as f64,
        histogram,
        count: n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> AnyValue {
        AnyValue::Str(v.to_owned())
    }

    /// Feed a hand-built four-segment fixture whose sigma is known by construction, and check every
    /// summary statistic against it.
    ///
    /// - `env=prod` is in all 4 segments  -> sigma 1.00
    /// - `env=staging` is in 2 segments   -> sigma 0.50
    /// - `pod=pod-0` is in segment 0 only -> sigma 0.25
    /// - `pod=pod-1`/`pod-2`/`pod-3` likewise, one segment each.
    #[test]
    fn sigma_matches_a_hand_built_four_segment_fixture() {
        let mut acc = Acc::new("logs", 64, 64, &[]);
        for seg in 0..4u32 {
            acc.begin_segment(i64::from(seg), i64::from(seg));
            acc.add_rows(10);
            // `env=prod` on every segment, several rows each (repeat must not inflate the count).
            for _ in 0..3 {
                acc.observe(AttrScope::Attributes, "env", &s("prod"), "prod", 1, true);
            }
            if seg < 2 {
                acc.observe(
                    AttrScope::Attributes,
                    "env",
                    &s("staging"),
                    "staging",
                    1,
                    true,
                );
            }
            // A pod name that lives in exactly one segment.
            let pod = format!("pod-{seg}");
            acc.observe(AttrScope::Attributes, "pod", &s(&pod), &pod, 7, true);
        }
        assert_eq!(acc.segments, 4);
        assert_eq!(acc.rows, 40);

        let pod = acc
            .keys
            .entry("pod", || unreachable!())
            .expect("pod tracked");
        assert_eq!(pod.values.tracked(), 4, "four distinct pod names");
        for (name, v) in pod.values.iter() {
            assert_eq!(v.segments, 1, "{name} must be in exactly one segment");
        }
        let pod_sigma =
            summarize(pod.values.iter().map(|(_, v)| v.segments), 4).expect("pod sigma");
        assert_eq!(pod_sigma.count, 4);
        assert_eq!(pod_sigma.p50, 0.25);
        assert_eq!(pod_sigma.p90, 0.25);
        assert_eq!(pod_sigma.max, 0.25);
        assert_eq!(pod_sigma.mean, 0.25);
        assert_eq!(pod_sigma.frac_le_25, 1.0);
        // Every value lands in the [0.2, 0.3) bucket.
        assert_eq!(pod_sigma.histogram[2], 4);
        assert_eq!(pod_sigma.histogram.iter().sum::<u32>(), 4);
        assert_eq!(pod.rows_present, 4 * 7);
        assert_eq!(pod.rows_string, 4 * 7);
        assert_eq!(pod.segments_present, 4);

        let env = acc
            .keys
            .entry("env", || unreachable!())
            .expect("env tracked");
        let mut counts: Vec<u32> = env.values.iter().map(|(_, v)| v.segments).collect();
        counts.sort_unstable();
        assert_eq!(counts, vec![2, 4], "staging in 2 segments, prod in all 4");
        let env_sigma = summarize(counts.iter().copied(), 4).expect("env sigma");
        assert_eq!(
            env_sigma.p50, 1.0,
            "p50 of [0.5, 1.0] rounds to the upper value"
        );
        assert_eq!(env_sigma.max, 1.0);
        assert_eq!(env_sigma.mean, 0.75);
        assert_eq!(env_sigma.frac_le_25, 0.0);
        assert_eq!(env_sigma.histogram[5], 1, "0.5 lands in [0.5,0.6)");
        assert_eq!(
            env_sigma.histogram[9], 1,
            "1.0 is clamped into the last bucket"
        );
        // `env=prod` was observed 3x per segment; the segment must still count once.
        assert_eq!(env.rows_present, 4 * 3 + 2);
    }

    /// Only `AnyValue::Str` counts toward `rows_string` — the promotion report must not credit a
    /// key whose values are numbers, because `lookup_promoted` would leave that column NULL.
    #[test]
    fn only_string_values_count_as_promotable() {
        let mut acc = Acc::new("logs", 64, 64, &[]);
        acc.begin_segment(0, 0);
        acc.observe(
            AttrScope::Attributes,
            "status",
            &AnyValue::Int(500),
            "500",
            3,
            true,
        );
        acc.observe(AttrScope::Attributes, "status", &s("500"), "500", 1, true);
        let k = acc
            .keys
            .entry("status", || unreachable!())
            .expect("tracked");
        assert_eq!(k.rows_present, 4);
        assert_eq!(k.rows_string, 1);
        assert_eq!(k.str_len_sum, 3);
        // `500` (int) and `"500"` (string) render to the same text, so they are one value here.
        assert_eq!(k.values.tracked(), 1);
    }

    /// Distinct non-string values must stay distinct.
    #[test]
    fn non_string_values_are_rendered_canonically() {
        assert_eq!(value_text(&AnyValue::Int(7)), "7");
        assert_eq!(value_text(&AnyValue::Bool(true)), "true");
        assert_eq!(value_text(&s("7")), "7");
    }

    /// The sampler must bound memory, keep complete counters for what it retains, and report the
    /// rate it fell back to.
    #[test]
    fn sampled_map_caps_memory_and_reports_the_rate() {
        let mut m: SampledMap<u32> = SampledMap::new(64);
        for i in 0..10_000 {
            if let Some(v) = m.entry(&format!("value-{i}"), || 0) {
                *v += 1;
            }
        }
        assert!(m.tracked() <= 64, "capped at 64, got {}", m.tracked());
        assert!(m.sample_rate() < 1.0, "the cap must have engaged");
        assert!(m.sample_rate() < 1.0);
        // Every retained key was retained from its first sight, so its counter is exact.
        for (k, v) in m.iter() {
            assert_eq!(*v, 1, "{k} counted more than once");
        }
        // The estimate should land within a factor of ~4 of the true 10k.
        let est = m.estimated_total();
        assert!(
            (2_500.0..40_000.0).contains(&est),
            "distinct estimate {est} is implausible"
        );
    }

    /// A retained value keeps a *complete* segment count even when the rate is cut mid-scan.
    #[test]
    fn retained_values_keep_complete_segment_counts() {
        let mut acc = Acc::new("logs", 8, 8, &[]);
        // 4 segments, each carrying the same 200 values, so every value's true sigma is 1.0.
        for seg in 0..4 {
            acc.begin_segment(seg, seg);
            for i in 0..200 {
                let v = format!("v{i}");
                acc.observe(AttrScope::Attributes, "k", &s(&v), &v, 1, true);
            }
        }
        let k = acc.keys.entry("k", || unreachable!()).expect("tracked");
        assert!(k.values.sample_rate() < 1.0);
        let sigma = summarize(k.values.iter().map(|(_, v)| v.segments), 4).expect("sigma");
        assert_eq!(sigma.max, 1.0);
        assert_eq!(
            sigma.mean, 1.0,
            "a value dropped by a later rate cut must not survive with a partial count"
        );
    }

    /// Long values must stay distinct, and cost the same as short ones.
    ///
    /// The predecessor folded values over 128 bytes to a hex digest so one kilobyte-sized attribute
    /// could not blow the memory the entry cap is supposed to bound. Keying the sketch by hash makes
    /// that special case unnecessary: *no* value text is stored, whatever its length.
    #[test]
    fn long_values_stay_distinct_without_being_stored() {
        let mut acc = Acc::new("logs", 8, 64, &[]);
        acc.begin_segment(0, 0);
        let long_a = "a".repeat(4096);
        let long_b = format!("{}b", "a".repeat(4095));
        acc.observe(AttrScope::Attributes, "k", &s(&long_a), &long_a, 1, true);
        acc.observe(AttrScope::Attributes, "k", &s(&long_b), &long_b, 1, true);
        // Repeat one of them: it must not become a third value.
        acc.observe(AttrScope::Attributes, "k", &s(&long_a), &long_a, 1, true);
        let k = acc.keys.entry("k", || unreachable!()).expect("tracked");
        assert_eq!(k.values.tracked(), 2, "two long values stay distinct");
        assert_eq!(k.rows_present, 3);
    }

    /// The window ladder against a fixture whose curve is known by construction.
    ///
    /// 8 segments, one per 30s over a 240s span, so a 60s window holds 2 segments and a 120s window
    /// holds 4:
    /// - `env=prod` is in every segment -> C is 1 at every scale; the curve is flat.
    /// - `pod` takes a fresh value per segment -> C(seg)=1, C(60s)=2, C(120s)=4, C(all)=8. The
    ///   curve tracks the window width exactly, which is what "fully localized" means.
    /// - `shard` cycles through 2 values every segment -> C(seg)=2 and stays 2. Locality 1.0 even
    ///   though it is not a constant: this is the case a global distinct count cannot tell from
    ///   `pod`'s first two points.
    #[test]
    fn the_window_ladder_tracks_a_known_curve() {
        const SEC: i64 = 1_000_000_000;
        let mut acc = Acc::new("logs", 64, 64, &[60 * SEC, 120 * SEC]);
        for seg in 0..8i64 {
            let t = seg * 30 * SEC;
            acc.begin_segment(t, t + 29 * SEC);
            acc.observe(AttrScope::Attributes, "env", &s("prod"), "prod", 5, true);
            let pod = format!("pod-{seg}");
            acc.observe(AttrScope::Attributes, "pod", &s(&pod), &pod, 5, true);
            for shard in 0..2 {
                let v = format!("shard-{shard}");
                acc.observe(AttrScope::Attributes, "shard", &s(&v), &v, 5, true);
            }
        }
        assert_eq!(acc.segments, 8);
        assert_eq!(
            acc.windows,
            vec![4, 2],
            "8 x 30s = 240s = 4 x 60s = 2 x 120s"
        );
        assert_eq!(acc.min_time, 0);
        assert_eq!(acc.max_time, 7 * 30 * SEC + 29 * SEC);

        // `C(w) = window postings / windows`, the same shape the report computes.
        let curve = |acc: &mut Acc, name: &str| -> Vec<f64> {
            let segments = f64::from(acc.segments);
            let windows = acc.windows.clone();
            let k = acc.keys.entry(name, || unreachable!()).expect("tracked");
            let mut out = vec![k.postings() as f64 / segments];
            out.extend(
                windows
                    .iter()
                    .enumerate()
                    .map(|(level, w)| k.window_postings(level) as f64 / f64::from(*w)),
            );
            out.push(k.values.tracked() as f64);
            out
        };
        assert_eq!(curve(&mut acc, "env"), vec![1.0, 1.0, 1.0, 1.0], "flat");
        assert_eq!(
            curve(&mut acc, "pod"),
            vec![1.0, 2.0, 4.0, 8.0],
            "localized"
        );
        assert_eq!(
            curve(&mut acc, "shard"),
            vec![2.0, 2.0, 2.0, 2.0],
            "flat at 2"
        );
    }

    /// A value repeated across every segment of one window must count that window **once** — the
    /// property the "last window ordinal" dedup exists to provide, and the one that breaks if
    /// segments are fed out of time order.
    #[test]
    fn a_window_is_counted_once_however_many_segments_touch_it() {
        const SEC: i64 = 1_000_000_000;
        let mut acc = Acc::new("logs", 64, 64, &[3600 * SEC]);
        for seg in 0..10i64 {
            acc.begin_segment(seg * SEC, seg * SEC + 1);
            acc.observe(AttrScope::Attributes, "env", &s("prod"), "prod", 100, true);
        }
        let k = acc.keys.entry("env", || unreachable!()).expect("tracked");
        assert_eq!(k.postings(), 10, "10 segments");
        assert_eq!(k.window_postings(0), 1, "all 10 fall in one hour");
        assert_eq!(acc.windows, vec![1]);
    }

    /// `--windows none` must leave no per-value ladder state at all.
    #[test]
    fn an_empty_ladder_costs_nothing_per_value() {
        let mut acc = Acc::new("logs", 8, 8, &[]);
        acc.begin_segment(0, 1);
        acc.observe(AttrScope::Attributes, "env", &s("prod"), "prod", 1, true);
        assert_eq!(acc.levels(), 0);
        assert!(acc.windows.is_empty());
        let k = acc.keys.entry("env", || unreachable!()).expect("tracked");
        for (_, v) in k.values.iter() {
            assert!(v.windows.is_empty());
        }
    }

    /// All permutations of `keys`, for the order-independence sweep below.
    fn permutations(keys: &[String]) -> Vec<Vec<String>> {
        if keys.len() <= 1 {
            return vec![keys.to_vec()];
        }
        let mut out = Vec::new();
        for i in 0..keys.len() {
            let mut rest = keys.to_vec();
            let head = rest.remove(i);
            for mut p in permutations(&rest) {
                p.insert(0, head.clone());
                out.push(p);
            }
        }
        out
    }

    fn scan(keys: &[String], cap: usize) -> SampledMap<u32> {
        let mut m: SampledMap<u32> = SampledMap::new(cap);
        for k in keys {
            if let Some(v) = m.entry(k, || 0) {
                *v += 1;
            }
        }
        m
    }

    /// The sketch's retained identities, in order. `iter` is already hash-ordered, but sorting
    /// makes the intent explicit and survives any future change to iteration order.
    fn sorted_hashes(m: &SampledMap<u32>) -> Vec<u64> {
        let mut v: Vec<u64> = m.iter().map(|(h, _)| h).collect();
        v.sort_unstable();
        v
    }

    /// **Below the cap, folding per-segment sketches equals one pass over everything.**
    ///
    /// This is the property a persisted per-`(segment, key)` sketch would rest on: the cardinality
    /// ladder becomes a *fold* over segment sketches instead of a stored count per bucket, so
    /// retention drops a segment's statistics with the segment and no bucket is written twice. Here
    /// the union fits under the cap, which is the regime that matters — a per-segment sketch is
    /// sized so a single segment rarely saturates it.
    #[test]
    fn folding_per_segment_sketches_equals_a_single_pass() {
        let keys: Vec<String> = (0..200).map(|i| format!("v{i}")).collect();
        let direct = scan(&keys, 4096);
        for parts in [2usize, 3, 8, 17, 200] {
            let mut folded: SampledMap<u32> = SampledMap::new(4096);
            for p in 0..parts {
                let part: Vec<String> = keys.iter().skip(p).step_by(parts).cloned().collect();
                folded.merge(scan(&part, 4096), |x, y| *x += y);
            }
            assert_eq!(
                sorted_hashes(&folded),
                sorted_hashes(&direct),
                "{parts}-way fold"
            );
            assert_eq!(folded.sample_rate(), 1.0);
            assert_eq!(folded.estimated_total(), 200.0);
            // Every key was seen exactly once, so every merged counter must be exactly 1 — a
            // double-count or a dropped observation would show up here.
            assert!(folded.iter().all(|(_, v)| *v == 1));
        }
    }

    /// **Above the cap, folding still equals a single pass — exactly.**
    ///
    /// This is what the bottom-k conversion bought. The predecessor could only manage *soundness*
    /// here (complete counters, a valid sample, the cap honoured) because there was no single
    /// direct-scan answer to be exact to. Now there is one, and the fold hits it: same entries, same
    /// counters, same rate, same estimate.
    #[test]
    fn folding_above_the_cap_equals_a_single_pass() {
        const CAP: usize = 64;
        const ROUNDS: u32 = 3;
        let keys: Vec<String> = (0..5_000).map(|i| format!("v{i}")).collect();

        // Direct: every key observed `ROUNDS` times in one accumulator.
        let mut direct: SampledMap<u32> = SampledMap::new(CAP);
        for _ in 0..ROUNDS {
            for k in &keys {
                if let Some(v) = direct.entry(k, || 0) {
                    *v += 1;
                }
            }
        }

        // Folded: three disjoint parts, each scanned separately, folded `ROUNDS` times over.
        let mut folded: SampledMap<u32> = SampledMap::new(CAP);
        for _ in 0..ROUNDS {
            for r in 0..3 {
                let part: Vec<String> = keys
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i % 3 == r)
                    .map(|(_, k)| k.clone())
                    .collect();
                folded.merge(scan(&part, CAP), |x, y| *x += y);
            }
        }

        assert!(direct.sample_rate() < 1.0, "the cap must have engaged");
        assert_eq!(folded.tracked(), CAP);
        assert_eq!(
            sorted_hashes(&folded),
            sorted_hashes(&direct),
            "same retained entries"
        );
        assert_eq!(folded.sample_rate(), direct.sample_rate(), "same rate");
        assert_eq!(
            folded.estimated_total(),
            direct.estimated_total(),
            "same estimate"
        );
        for (h, count) in folded.iter() {
            assert_eq!(*count, ROUNDS, "{h:#x} has a partial or doubled count");
        }
        let est = folded.estimated_total();
        assert!(
            (3_500.0..7_000.0).contains(&est),
            "estimate {est} is implausible for 5,000 distinct values"
        );
    }

    /// **The sample does not depend on arrival order** — the property the bottom-k conversion was
    /// for, checked exhaustively over every permutation of several key families at several caps.
    ///
    /// The predecessor failed this: it halved its rate whenever the map was full at the moment a
    /// key arrived, so different orders reached different rates, kept different keys, and reported
    /// different `estimated_total`s over identical input. This same search found tens of thousands
    /// of disagreeing pairs then. It must find none now.
    #[test]
    fn the_sample_is_independent_of_arrival_order() {
        let mut checked = 0usize;
        let mut capped = 0usize;
        for family in 0..30u32 {
            for n in [3usize, 4, 5, 6] {
                let keys: Vec<String> = (0..n).map(|i| format!("f{family}-x{i}")).collect();
                for cap in [2usize, 3, 4] {
                    let mut expected: Option<(Vec<u64>, f64, f64)> = None;
                    for p in permutations(&keys) {
                        let m = scan(&p, cap);
                        let got = (sorted_hashes(&m), m.sample_rate(), m.estimated_total());
                        match &expected {
                            None => {
                                if got.1 < 1.0 {
                                    capped += 1;
                                }
                                expected = Some(got);
                            }
                            Some(want) => assert_eq!(
                                got, *want,
                                "order changed the sample: family={family} n={n} cap={cap} order={p:?}"
                            ),
                        }
                        checked += 1;
                    }
                }
            }
        }
        // 30 families x 3 caps x (3! + 4! + 5! + 6!) orderings.
        assert_eq!(checked, 30 * 3 * (6 + 24 + 120 + 720));
        assert!(capped > 0, "some combination must actually engage the cap");
    }

    #[test]
    fn summarize_needs_segments_and_values() {
        assert!(summarize([1u32].into_iter(), 0).is_none());
        assert!(summarize([].into_iter(), 4).is_none());
    }
}
