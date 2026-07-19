//! `imbhd` — the reference imbh HTTP server binary (ARCHITECTURE.md §10.16).
//!
//! Usage: `imbhd [DB_DIR] [ADDR] [GRPC_ADDR]` (defaults: `./imbh-data`, `127.0.0.1:4318`,
//! `127.0.0.1:4317`). Point a stock OTel SDK's OTLP/HTTP exporter at `http://ADDR` and query via
//! `POST /api/query` with a SQL body. `GRPC_ADDR` is only used when built with `--features grpc`,
//! which additionally serves OTLP/gRPC (the OTel SDK default) on that port.
//!
//! Self-observability is opt-in at build time: `cargo build -p imbh-server --features tracing`
//! installs a `tracing-subscriber` fmt layer that renders imbh's internal spans/events to stderr.
//! Filter with `RUST_LOG` (e.g. `RUST_LOG=imbh=debug`); it defaults to `info` when unset. The
//! default build carries no `tracing` dependency at all (ARCHITECTURE.md §11 footprint gate).

use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    // Render imbh's internal instrumentation to stderr via the facade's `console` collector: it is
    // RUST_LOG-aware and, absent RUST_LOG, defaults every imbh target to `info` so the startup banner
    // and request spans show without extra setup.
    #[cfg(feature = "tracing")]
    imbh::console::init();

    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "./imbh-data".to_owned());
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:4318".to_owned());
    #[cfg(feature = "grpc")]
    let grpc_addr = args.next().unwrap_or_else(|| "127.0.0.1:4317".to_owned());

    let db = imbh::Db::builder(&dir).open()?;

    // With `tracing` on, the banner flows through the subscriber as structured events; otherwise the
    // default build keeps the plain stdout banner so `imbhd` is still self-describing.
    #[cfg(feature = "tracing")]
    {
        tracing::info!(%addr, %dir, "imbhd listening");
        tracing::info!("OTLP/HTTP: POST /v1/logs, /v1/traces, /v1/metrics");
        tracing::info!("query: POST /api/query (SQL body -> JSON)");
        #[cfg(feature = "grpc")]
        tracing::info!(%grpc_addr, "OTLP/gRPC: Logs/Trace/Metrics Service Export");
    }
    #[cfg(not(feature = "tracing"))]
    {
        println!("imbhd listening on http://{addr}  (data dir: {dir})");
        println!("  OTLP/HTTP: POST /v1/logs · /v1/traces · /v1/metrics");
        println!("  query:     POST /api/query  (SQL body → JSON)");
        #[cfg(feature = "grpc")]
        println!("  OTLP/gRPC: {grpc_addr}  (Logs/Trace/Metrics Service Export)");
    }

    // Without `grpc`, the HTTP server owns the main thread and blocks forever (unchanged default).
    // With `grpc`, run OTLP/gRPC in the foreground and the std-net HTTP server on a background thread;
    // both share the one `Arc<Db>`. If either accept loop returns (only on a bind/serve error), the
    // process exits with that error.
    #[cfg(not(feature = "grpc"))]
    imbh_server::serve(db, &addr)?;
    #[cfg(feature = "grpc")]
    {
        let http_db = db.clone();
        let http_addr = addr.clone();
        std::thread::spawn(move || {
            if let Err(e) = imbh_server::serve(http_db, &http_addr) {
                eprintln!("imbhd HTTP server error: {e}");
            }
        });
        imbh_server::grpc::serve_grpc_blocking(db, &grpc_addr)?;
    }
    Ok(())
}
