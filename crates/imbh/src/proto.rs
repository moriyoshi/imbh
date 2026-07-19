//! Protobuf wire types for the query-API inputs, plus the mappings onto the typed builders
//! (ARCHITECTURE.md §10.17). Enabled by the `proto` feature.
//!
//! This is the public facade: it re-exports the generated wire types from [`imbh_proto`] (so hosts
//! name them as `imbh::proto::LogQuery`, `imbh::proto::TimeRange`, …) and [`encode_query_stats`].
//! The `TryFrom<imbh_proto::…>` conversions onto the typed builders are implemented in
//! `crate::proto_impl` and are in scope automatically:
//!
//! ```ignore
//! let q = imbh::LogQuery::try_from(pb_query)?;  // pb_query: imbh::proto::LogQuery
//! let (batches, stats) = db.logs().query_batches_with_stats(q).await?;
//! ```

pub use crate::proto_impl::encode_query_stats;
pub use imbh_proto::*;
