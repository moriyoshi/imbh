//! `imbh-tracing` — in-process `tracing` → imbh plumbing.
//!
//! **Db sink.** [`DbLayer`] — a [`tracing_subscriber::Layer`] that ingests `tracing` events → the
//! `logs` table and span closes → the `spans` table of an embedded [`imbh::Db`], in-process, over
//! the same OTLP ingest path a network exporter would hit. This is the self-observation story: your
//! app's (and imbh's own) `tracing` lands in imbh with zero hops.
//!
//! The companion stderr *renderer* (a `fmt` subscriber that prints imbh's instrumentation to the
//! terminal) lives in the `imbh` facade as `imbh::console`, behind its off-by-default
//! `tracing-console` feature — the two are independent and compose on the same registry.
//!
//! ```ignore
//! // Db sink: compose with the tracing-subscriber registry.
//! use tracing_subscriber::prelude::*;
//! let db = imbh::Db::in_memory().open()?;
//! tracing_subscriber::registry()
//!     .with(imbh_tracing::DbLayer::new(db.clone()).with_service("checkout"))
//!     .init();
//! // ... emit spans/events anywhere; query them back via `db.sql("SELECT … FROM logs/spans")`.
//! ```

mod layer;
pub use layer::DbLayer;

pub use tracing_subscriber;
