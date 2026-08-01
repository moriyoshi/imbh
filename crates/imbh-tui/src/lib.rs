//! A read-only terminal explorer for an imbh database.
//!
//! [`run`] takes an open [`Db`](imbh::Db) and drives a full-screen TUI over it: metrics, traces, and
//! logs, each with its own query box, plus the detail views they drill into. Everything is read-only
//! — the explorer never writes to the database.
//!
//! The crate is laid out as ingest-free layers over that database:
//!
//! * [`model`] — the view model (routes, snapshots, catalog nodes, update messages).
//! * [`app`] — [`App`](app::App), the state machine the key handler drives.
//! * [`keys`] / [`runtime`] / [`terminal`] — the event loop and the terminal it owns.
//! * [`fetch`] / [`tasks`] — the queries behind a refresh, dispatched off the event-loop thread.
//! * [`ui`] — rendering; it reads [`App`](app::App) and never mutates it.
//! * [`syntax`] / [`completion`] / [`promql`] — the query-language support behind the editor.
//! * [`format`] / [`time`] / [`waterfall`] / [`detail_text`] / [`chart`] — display helpers.
//! * [`mascot`] — the animated easter egg that rides on top of it all.

mod app;
mod chart;
mod completion;
mod detail_text;
mod fetch;
mod format;
mod keys;
mod mascot;
mod model;
mod promql;
mod runtime;
mod syntax;
mod tasks;
mod terminal;
mod time;
mod ui;
mod waterfall;

#[cfg(test)]
mod testutil;

pub use model::Options;
pub use runtime::run;
pub use time::parse_datetime;
