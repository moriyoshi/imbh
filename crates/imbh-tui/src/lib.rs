//! A read-only terminal explorer for an imbh database.
//!
//! [`run`] drives a full-screen TUI over a [`Backend`]: metrics, traces, and logs, each with its own
//! query box, plus the detail views they drill into. Everything is read-only — the explorer never
//! writes to the database.
//!
//! The backend is either an imbh directory opened in-process or, with `--url`, a running `imbhd`
//! reached over the head API (ARCHITECTURE.md §10.19) — which is how the explorer becomes a *head*:
//! a UI with no database of its own, able to show a writer's live buffer and to sit on a different
//! machine from the data. See [`backend`] for what the two modes share, which is everything but the
//! transport.
//!
//! The crate is laid out as ingest-free layers over that backend:
//!
//! * [`backend`] — where the answers come from, local or remote.
//! * [`model`] — the view model (routes, snapshots, catalog nodes, update messages).
//! * [`app`] — [`App`](app::App), the state machine the key handler drives.
//! * [`keys`] / [`runtime`] / [`terminal`] — the event loop and the terminal it owns.
//! * [`fetch`] / [`tasks`] — the queries behind a refresh, dispatched off the event-loop thread.
//! * [`ui`] — rendering; it reads [`App`](app::App) and never mutates it.
//! * [`syntax`] / [`completion`] / [`promql`] — the query-language support behind the editor.
//! * [`format`] / [`time`] / [`waterfall`] / [`detail_text`] / [`chart`] — display helpers.
//! * [`mascot`] — the animated easter egg that rides on top of it all.
//!
//! The binary this crate backs is also imbh's **MCP server over stdio** (`imbh-tui --mcp-stdio`):
//! the same read-only view of someone else's database, addressed to an agent rather than to a
//! person. The protocol and tools live in `imbh-mcp`; [`cli`] is where the two modes are told apart.

pub mod backend;
pub mod cli;

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

pub use backend::Backend;
pub use model::Options;
pub use runtime::run;
pub use time::parse_datetime;
