//! Signal / table / metric-kind enums (ARCHITECTURE.md §10.4). The full trace/metric schemas
//! arrive in M2/M3; the variants are defined now so the type surface is stable.

/// The three OTel signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Logs,
    Traces,
    Metrics,
}

/// A physical table. All seven are implemented: logs (M1), spans (M2), and the five metric
/// families (M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Table {
    Logs,
    Spans,
    MetricsGauge,
    MetricsSum,
    MetricsHistogram,
    MetricsExpHistogram,
    MetricsSummary,
}

impl Table {
    /// Every physical table, in a stable order (logs, spans, then the five metric families).
    /// Handy for cross-signal sweeps (e.g. attribute discovery) that must touch all tables.
    pub const ALL: [Table; 7] = [
        Table::Logs,
        Table::Spans,
        Table::MetricsGauge,
        Table::MetricsSum,
        Table::MetricsHistogram,
        Table::MetricsExpHistogram,
        Table::MetricsSummary,
    ];

    /// The table name as used in SQL and on-disk partition paths.
    pub fn as_str(&self) -> &'static str {
        match self {
            Table::Logs => "logs",
            Table::Spans => "spans",
            Table::MetricsGauge => "metrics_gauge",
            Table::MetricsSum => "metrics_sum",
            Table::MetricsHistogram => "metrics_histogram",
            Table::MetricsExpHistogram => "metrics_exp_histogram",
            Table::MetricsSummary => "metrics_summary",
        }
    }
}

/// OTel metric point kinds (ARCHITECTURE.md §6.4). Defined for the stable surface; M3 implements them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Gauge,
    Sum,
    Histogram,
    ExpHistogram,
    Summary,
}

/// OTel severity number, 1..=24 (ARCHITECTURE.md §10.4). The associated constants name the band floors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SeverityNumber(pub u8);

impl SeverityNumber {
    pub const TRACE: SeverityNumber = SeverityNumber(1);
    pub const DEBUG: SeverityNumber = SeverityNumber(5);
    pub const INFO: SeverityNumber = SeverityNumber(9);
    pub const WARN: SeverityNumber = SeverityNumber(13);
    pub const ERROR: SeverityNumber = SeverityNumber(17);
    pub const FATAL: SeverityNumber = SeverityNumber(21);
}
