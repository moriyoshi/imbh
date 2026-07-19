//! Time vocabulary (ARCHITECTURE.md §10.4). Nanoseconds since the Unix epoch, UTC — matches Arrow's
//! `Timestamp(Nanosecond)` storage exactly, so buffer↔DTO conversion is lossless.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Nanoseconds since the Unix epoch, UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn from_unix_nanos(ns: i64) -> Self {
        Timestamp(ns)
    }

    pub fn unix_nanos(&self) -> i64 {
        self.0
    }

    /// The current wall-clock instant.
    pub fn now() -> Self {
        let ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        Timestamp(ns)
    }
}

/// Elapsed nanoseconds. Serializes as an integer (ARCHITECTURE.md §10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DurationNs(pub u64);

/// Scan direction for log queries (ARCHITECTURE.md §10.4). Logs default to newest-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Direction {
    #[default]
    Backward,
    Forward,
}

/// A half-open UTC time window `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TimeRange {
    pub start: Timestamp,
    pub end: Timestamp,
}

impl TimeRange {
    pub fn between(start: Timestamp, end: Timestamp) -> Self {
        TimeRange { start, end }
    }

    /// `now - d ..= now` (Loki `since`).
    pub fn since(d: Duration) -> Self {
        let now = Timestamp::now();
        TimeRange {
            start: Timestamp(now.0.saturating_sub(d.as_nanos() as i64)),
            end: Timestamp(now.0.saturating_add(1)),
        }
    }

    /// Unbounded (retention still applies).
    pub fn all() -> Self {
        TimeRange {
            start: Timestamp(i64::MIN),
            end: Timestamp(i64::MAX),
        }
    }
}
