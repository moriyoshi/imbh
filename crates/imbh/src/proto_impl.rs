//! Protobuf wire types for the query-API inputs, and their mappings onto the typed builders
//! (ARCHITECTURE.md §10.17, "Phase 0" of the Go / FFI binding).
//!
//! This module re-exports the generated wire types from [`imbh_proto`] under `imbh::proto` and adds
//! `TryFrom<imbh_proto::…>` for each typed builder ([`crate::LogQuery`], [`crate::MetricQuery`], …),
//! so a host decodes a protobuf request and turns it into a builder in one step:
//!
//! ```ignore
//! let q = imbh::LogQuery::try_from(pb_log_query)?;   // pb_log_query: imbh::proto::LogQuery
//! let (batches, stats) = db.logs().query_batches_with_stats(q).await?;
//! ```
//!
//! The mappings go through the builders' **public setters** (plus the `pub(crate)` page cursor), so
//! they never depend on private field layout. Conversions are fallible only where the wire type is
//! wider than the domain type — an out-of-range enum discriminant, a severity above 255, a negative
//! duration, or a length that overflows `usize` — each surfaced as a user-facing request error.
//!
//! Results are **not** modeled here: bulk result rows leave as Arrow (`*_batches` query methods),
//! and only the small [`QueryStats`] metadata envelope is encoded back to protobuf
//! ([`encode_query_stats`]).
//!
//! This is the private implementation module; the public facade lives at [`crate::proto`], which
//! re-exports the wire types and [`encode_query_stats`]. The `TryFrom` impls here are visible
//! crate-wide regardless (trait impls are never module-private).

use std::time::Duration;

use imbh_proto as pb;

use crate::{
    Aggregation, Direction, Error, ExpHistogramQuery, HistogramQuery, LogQuery, MetricQuery,
    PageCursor, QueryStats, Result, SeverityNumber, SpanMetricsQuery, TimeRange, Timestamp,
    TraceQuery,
};

/// A malformed query request is a user error (the wire message carried a value outside the domain
/// type's range). Reuses the query-layer user-error class so `Error::is_user_error()` holds.
fn bad_request(msg: impl Into<String>) -> Error {
    Error::query_msg(msg)
}

/// `imbh_proto::TimeRange` → the domain window (`i64::MIN`/`MAX` still mean unbounded).
fn time_range(p: pb::TimeRange) -> TimeRange {
    TimeRange::between(Timestamp(p.start), Timestamp(p.end))
}

/// A non-negative nanosecond count → `Duration` (a negative wire value is a request error).
fn duration(nanos: i64) -> Result<Duration> {
    u64::try_from(nanos)
        .map(Duration::from_nanos)
        .map_err(|_| bad_request(format!("negative step_nanos {nanos}")))
}

fn direction(v: i32) -> Result<Direction> {
    match pb::Direction::try_from(v) {
        Ok(pb::Direction::Backward) => Ok(Direction::Backward),
        Ok(pb::Direction::Forward) => Ok(Direction::Forward),
        Err(_) => Err(bad_request(format!("invalid Direction discriminant {v}"))),
    }
}

fn aggregation(v: i32) -> Result<Aggregation> {
    match pb::Aggregation::try_from(v) {
        Ok(pb::Aggregation::Sum) => Ok(Aggregation::Sum),
        Ok(pb::Aggregation::Avg) => Ok(Aggregation::Avg),
        Ok(pb::Aggregation::Min) => Ok(Aggregation::Min),
        Ok(pb::Aggregation::Max) => Ok(Aggregation::Max),
        Ok(pb::Aggregation::Count) => Ok(Aggregation::Count),
        Err(_) => Err(bad_request(format!("invalid Aggregation discriminant {v}"))),
    }
}

/// Apply the shared attribute predicates (present on both log and trace queries) via the builder's
/// public setters. `$p` is the wire message; `$q` the builder being folded.
macro_rules! apply_attr_common {
    ($q:expr, $p:expr) => {{
        let mut q = $q;
        for kv in &$p.attr_eq {
            q = q.attr_eq(&kv.key, &kv.value);
        }
        for k in &$p.attr_exists {
            q = q.attr_exists(k);
        }
        for kv in &$p.attr_matches {
            q = q.attr_matches(&kv.key, &kv.value);
        }
        for kvs in &$p.attr_in {
            let vals: Vec<&str> = kvs.values.iter().map(String::as_str).collect();
            q = q.attr_in(&kvs.key, &vals);
        }
        for kvs in &$p.attr_not_in {
            let vals: Vec<&str> = kvs.values.iter().map(String::as_str).collect();
            q = q.attr_not_in(&kvs.key, &vals);
        }
        for kv in &$p.attr_regex {
            q = q.attr_regex(&kv.key, &kv.value);
        }
        q
    }};
}

/// Fold the numeric-attribute filters (`attr_gt`/`ge`/`lt`/`le`) — shared by log and trace queries.
macro_rules! apply_attr_num {
    ($q:expr, $filters:expr) => {{
        let mut q = $q;
        for f in $filters {
            let op = pb::NumOp::try_from(f.op)
                .map_err(|_| bad_request(format!("invalid NumOp discriminant {}", f.op)))?;
            q = match op {
                pb::NumOp::Gt => q.attr_gt(&f.key, f.value),
                pb::NumOp::Ge => q.attr_ge(&f.key, f.value),
                pb::NumOp::Lt => q.attr_lt(&f.key, f.value),
                pb::NumOp::Le => q.attr_le(&f.key, f.value),
            };
        }
        q
    }};
}

/// Fold the PromQL-style label selectors (`filter`/`filter_ne`/`filter_regex`/`filter_not_regex`) —
/// shared by the three metric-query builders (identical method names on each).
macro_rules! apply_label_filters {
    ($q:expr, $filters:expr) => {{
        let mut q = $q;
        for f in $filters {
            let op = pb::LabelOp::try_from(f.op)
                .map_err(|_| bad_request(format!("invalid LabelOp discriminant {}", f.op)))?;
            q = match op {
                pb::LabelOp::Eq => q.filter(&f.key, &f.value),
                pb::LabelOp::Ne => q.filter_ne(&f.key, &f.value),
                pb::LabelOp::Regex => q.filter_regex(&f.key, &f.value),
                pb::LabelOp::NotRegex => q.filter_not_regex(&f.key, &f.value),
            };
        }
        q
    }};
}

impl TryFrom<pb::LogQuery> for LogQuery {
    type Error = Error;

    fn try_from(p: pb::LogQuery) -> Result<Self> {
        let mut q = LogQuery::new();
        if let Some(s) = &p.service {
            q = q.service(s);
        }
        if let Some(sev) = p.min_severity {
            let sev = u8::try_from(sev)
                .map_err(|_| bad_request(format!("min_severity {sev} exceeds 255")))?;
            q = q.severity_at_least(SeverityNumber(sev));
        }
        if let Some(t) = &p.text {
            q = q.matches(t);
        }
        q = apply_attr_common!(q, p);
        q = apply_attr_num!(q, &p.attr_num);
        if let Some(r) = p.range {
            q = q.range(time_range(r));
        }
        // limit 0 → keep the builder default (100); a real cap is any positive value.
        if p.limit != 0 {
            q = q.limit(usize::try_from(p.limit).map_err(|_| bad_request("limit exceeds usize"))?);
        }
        q = q.direction(direction(p.direction)?);
        if p.offset != 0 {
            let offset =
                usize::try_from(p.offset).map_err(|_| bad_request("offset exceeds usize"))?;
            q = q.after(PageCursor(offset));
        }
        Ok(q)
    }
}

impl TryFrom<pb::TraceQuery> for TraceQuery {
    type Error = Error;

    fn try_from(p: pb::TraceQuery) -> Result<Self> {
        let mut q = TraceQuery::new();
        if let Some(s) = &p.service {
            q = q.service(s);
        }
        if let Some(n) = &p.name {
            q = q.name(n);
        }
        if let Some(t) = &p.text {
            q = q.matches(t);
        }
        if let Some(d) = p.min_duration_ns {
            q = q.min_duration(Duration::from_nanos(d));
        }
        if let Some(d) = p.max_duration_ns {
            q = q.max_duration(Duration::from_nanos(d));
        }
        if let Some(s) = &p.status {
            q = q.status(s);
        }
        if let Some(k) = &p.kind {
            q = q.kind(k);
        }
        q = apply_attr_common!(q, p);
        q = apply_attr_num!(q, &p.attr_num);
        if let Some(r) = p.range {
            q = q.range(time_range(r));
        }
        if p.limit != 0 {
            q = q.limit(usize::try_from(p.limit).map_err(|_| bad_request("limit exceeds usize"))?);
        }
        Ok(q)
    }
}

impl TryFrom<pb::SpanMetricsQuery> for SpanMetricsQuery {
    type Error = Error;

    fn try_from(p: pb::SpanMetricsQuery) -> Result<Self> {
        let mut q = SpanMetricsQuery::new();
        if let Some(s) = &p.service {
            q = q.service(s);
        }
        if let Some(n) = &p.name {
            q = q.name(n);
        }
        if let Some(k) = &p.kind {
            q = q.kind(k);
        }
        if let Some(s) = &p.status {
            q = q.status(s);
        }
        for kv in &p.attr_eq {
            q = q.attr_eq(&kv.key, &kv.value);
        }
        for k in &p.group_by {
            q = q.group_by(k);
        }
        if let Some(r) = p.range {
            q = q.range(time_range(r));
        }
        if let Some(ns) = p.step_nanos {
            q = q.step(duration(ns)?);
        }
        Ok(q)
    }
}

impl TryFrom<pb::MetricQuery> for MetricQuery {
    type Error = Error;

    fn try_from(p: pb::MetricQuery) -> Result<Self> {
        let table = pb::MetricTable::try_from(p.table)
            .map_err(|_| bad_request(format!("invalid MetricTable discriminant {}", p.table)))?;
        let mut q = match table {
            pb::MetricTable::Gauge => MetricQuery::gauge(&p.metric),
            pb::MetricTable::Sum => MetricQuery::sum(&p.metric),
        };
        // Absent aggregation keeps the family default (gauge → avg, sum → sum).
        if let Some(a) = p.aggregation {
            q = q.aggregation(aggregation(a)?);
        }
        for k in &p.group_by {
            q = q.group_by(k);
        }
        q = apply_label_filters!(q, &p.filters);
        if let Some(r) = p.range {
            q = q.range(time_range(r));
        }
        if let Some(ns) = p.step_nanos {
            q = q.step(duration(ns)?);
        }
        q = match pb::RateMode::try_from(p.rate)
            .map_err(|_| bad_request(format!("invalid RateMode discriminant {}", p.rate)))?
        {
            pb::RateMode::Off => q,
            pb::RateMode::Delta => q.rate(),
            pb::RateMode::Counter => q.rate_counter(),
        };
        Ok(q)
    }
}

impl TryFrom<pb::HistogramQuery> for HistogramQuery {
    type Error = Error;

    fn try_from(p: pb::HistogramQuery) -> Result<Self> {
        let mut q = HistogramQuery::new(&p.metric);
        if let Some(phi) = p.phi {
            q = q.quantile(phi);
        }
        for k in &p.group_by {
            q = q.group_by(k);
        }
        q = apply_label_filters!(q, &p.filters);
        if let Some(r) = p.range {
            q = q.range(time_range(r));
        }
        if let Some(ns) = p.step_nanos {
            q = q.step(duration(ns)?);
        }
        Ok(q)
    }
}

impl TryFrom<pb::ExpHistogramQuery> for ExpHistogramQuery {
    type Error = Error;

    fn try_from(p: pb::ExpHistogramQuery) -> Result<Self> {
        let mut q = ExpHistogramQuery::new(&p.metric);
        if let Some(phi) = p.phi {
            q = q.quantile(phi);
        }
        for k in &p.group_by {
            q = q.group_by(k);
        }
        q = apply_label_filters!(q, &p.filters);
        if let Some(r) = p.range {
            q = q.range(time_range(r));
        }
        if let Some(ns) = p.step_nanos {
            q = q.step(duration(ns)?);
        }
        Ok(q)
    }
}

/// Encode read-side scan statistics into the protobuf result envelope. The bulk result rows travel
/// as Arrow, out of band; this is the small metadata companion a binding returns alongside them.
pub fn encode_query_stats(s: &QueryStats) -> pb::QueryStats {
    pb::QueryStats {
        segments_scanned: s.segments_scanned,
        segments_pruned: s.segments_pruned,
        rows_scanned: s.rows_scanned,
        rows_returned: s.rows_returned,
        bytes_scanned: s.bytes_scanned,
        elapsed_ns: s.elapsed.0,
        used_index: s.used_index,
    }
}
