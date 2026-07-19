//! Console collector — a stderr `fmt` subscriber for imbh's own instrumentation.
//!
//! A [`tracing_subscriber`] `fmt` layer whose default filter understands imbh's per-crate targets
//! and honors `RUST_LOG` — [`init`] plus the [`env_filter`] / [`directives`] building blocks. It
//! renders the spans/events imbh emits when built with `imbh/tracing` to the terminal.
//!
//! This is the renderer half of imbh's self-observation story; it is separate from the `DbLayer`
//! sink in the `imbh-tracing` helper crate, which routes `tracing` back into an embedded [`Db`].
//! Gated behind the off-by-default `tracing-console` feature so the `tracing-subscriber` subtree
//! stays out of the default library graph (ARCHITECTURE.md §11 / §12 footprint containment).
//!
//! ```no_run
//! // Console: stderr, every imbh target at INFO unless RUST_LOG overrides.
//! imbh::console::init();
//! ```
//!
//! ## Why a default filter that lists every crate
//!
//! Each imbh crate is its own `tracing` target, so a lone `imbh=debug` directive covers only the
//! `imbh` facade — not `imbh_storage`, `imbh_query`, and the rest — and the reference binary's own
//! events log under the `imbhd` target. The default filter here enables all of [`IMBH_TARGETS`]
//! together so a host sees the whole pipeline without enumerating them by hand.
//!
//! [`Db`]: crate::Db

use tracing_subscriber::EnvFilter;

/// Every imbh library and binary target, spelled as `tracing` / `RUST_LOG` names (a crate's `-`
/// becomes `_`). Emission from `imbh-otlp`, `imbh-storage`, `imbh-index`, and `imbh-query` is
/// feature-gated behind `imbh/tracing`; the `imbhd` entry covers the reference binary's own events.
pub const IMBH_TARGETS: &[&str] = &[
    "imbh",
    "imbh_otlp",
    "imbh_storage",
    "imbh_index",
    "imbh_query",
    "imbh_server",
    "imbhd",
];

/// A comma-separated `RUST_LOG`-style directive string enabling every [`IMBH_TARGETS`] entry at
/// `level` — e.g. `directives("debug")` yields `"imbh=debug,imbh_otlp=debug,…,imbhd=debug"`.
pub fn directives(level: &str) -> String {
    IMBH_TARGETS
        .iter()
        .map(|t| format!("{t}={level}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Build an [`EnvFilter`]: `RUST_LOG` verbatim when it is set, otherwise every imbh target at
/// `default_level` (via [`directives`]). Non-imbh targets (DataFusion, Tantivy, …) stay silent
/// unless `RUST_LOG` opts them in.
pub fn env_filter(default_level: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(directives(default_level)))
}

/// Install a global `fmt` subscriber writing to **stderr** (keeping stdout clean for piped output),
/// filtered by [`env_filter`]. Returns an error if a global subscriber is already set — install
/// exactly once, at startup.
pub fn try_init_with(
    default_level: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter(default_level))
        .try_init()
}

/// [`try_init_with`] with an `info` default level.
pub fn try_init() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    try_init_with("info")
}

/// Install the stderr subscriber at `default_level`, panicking if a global subscriber is already
/// set. The common one-liner for `main`; use [`try_init_with`] when one may already exist.
pub fn init_with(default_level: &str) {
    try_init_with(default_level)
        .expect("imbh::console: a global tracing subscriber is already set");
}

/// [`init_with`] with an `info` default level.
pub fn init() {
    init_with("info");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directives_cover_every_target_once() {
        let d = directives("debug");
        for t in IMBH_TARGETS {
            assert!(d.contains(&format!("{t}=debug")), "missing target {t}");
        }
        // Comma-joined with no trailing/leading comma.
        assert_eq!(d.matches(',').count(), IMBH_TARGETS.len() - 1);
        assert!(!d.starts_with(',') && !d.ends_with(','));
    }

    #[test]
    fn env_filter_builds() {
        // Builds a usable filter regardless of the ambient RUST_LOG (set → honored; unset →
        // imbh directives). We only assert it constructs without panicking.
        let _ = env_filter("info");
    }
}
