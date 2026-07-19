//! imbh-core — the arrow-free foundation crate.
//!
//! Holds imbh's pure domain types: the OTel value model ([`AnyValue`]), the shared
//! canonical-JSON encoder (ARCHITECTURE.md §6.1), the normalized ingest row ([`LogRow`]),
//! ids, config, the error model, and manifest types. It deliberately does **not**
//! depend on arrow, parquet, DataFusion, Tantivy, or serde — those live behind the
//! `imbh-storage` / `imbh-query` / `imbh-index` boundaries (ARCHITECTURE.md §12).

mod attributes;
mod canonical;
mod config;
mod enums;
mod error;
mod histogram;
mod ids;
mod json;
mod manifest;
mod row;
mod text;
mod time;
mod value;

pub use attributes::{Attributes, json_get};
pub use canonical::{canonical_json_object, canonical_json_value};
pub use config::{
    Access, Compression, Ingest, Maintenance, MemoryBudget, Overflow, Promote, Refresh, Retention,
    WalMode,
};
pub use enums::{MetricKind, SeverityNumber, Signal, Table};
pub use error::{
    ConfigError, ConfigKind, Error, IngestError, IngestKind, OpenError, OpenKind, ParquetPhase,
    QueryError, QueryKind, Result, StorageError, StorageKind, WalPhase,
};
pub use histogram::{exp_histogram_quantile, histogram_quantile};
pub use ids::{Lsn, SpanId, TraceId};
pub use json::parse as parse_json;
pub use manifest::SegmentRef;
pub use row::{ExpHistogramRow, HistogramRow, LogRow, ScalarMetricRow, SpanRow, SummaryRow};
pub use text::{matches_terms, tokenize};
pub use time::{Direction, DurationNs, TimeRange, Timestamp};
pub use value::AnyValue;
