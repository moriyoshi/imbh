//! Parser- and engine-independent expression models and reference evaluators for the LGTM query
//! languages. These modules reference each other's public types through the crate-root re-exports in
//! [`crate`], so they carry no dependency on the [`crate::syntax`] parser layer.

pub mod common;
pub mod logql;
pub mod promql;
pub mod traceql;
