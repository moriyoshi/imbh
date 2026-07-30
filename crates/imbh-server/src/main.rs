//! `imbhd` — the reference imbh HTTP server binary (ARCHITECTURE.md §10.16).
//!
//! Usage: `imbhd [DB_DIR] [ADDR] [GRPC_ADDR]` (defaults: `./imbh-data`, `127.0.0.1:4318`,
//! `127.0.0.1:4317`). Point a stock OTel SDK's OTLP/HTTP exporter at `http://ADDR` and query via
//! `POST /api/query` with a SQL body. `GRPC_ADDR` is only used when built with `--features grpc`,
//! which additionally serves OTLP/gRPC (the OTel SDK default) on that port.
//!
//! Both addresses also read from the environment — `IMBH_LISTEN_ADDR` and `IMBH_GRPC_LISTEN_ADDR` —
//! which is how a managed Docker plugin tunes them, since a plugin's entrypoint args are frozen in
//! its `config.json` while `env` entries are changeable with `docker plugin set`. Setting either to
//! the **empty string** disables that listener; disabling both leaves a process with no network port
//! at all, which is a supported configuration when the Docker plugin endpoint is on.
//!
//! `imbhd` runs a **flush scheduler** (the library leaves that to the host; a collector process wants
//! one). `IMBH_FLUSH` picks the strategy — a spec of triggers that OR together, e.g.
//! `interval=5s,buffer=16MiB,rows=50000,wal=64MiB,idle=2s`, or `manual` to seal only on
//! `POST /admin/flush` and shutdown; the default is `interval=5s` plus the engine's memory-budget byte
//! threshold. `IMBH_MAINTENANCE_INTERVAL` (default `60s`) sets how often retention runs. Until the
//! buffer is sealed, rows live in the WAL + memory only, so this is also what bounds `imbhd`'s RSS and
//! WAL growth.
//!
//! Built with `--features docker`, `imbhd` additionally serves the Docker logging-driver plugin API
//! when `IMBH_DOCKER_PLUGIN_SOCKET` is set (a managed plugin's `config.json` sets it to
//! `/run/docker/plugins/imbh.sock`); container output then lands in the same DB the query endpoint
//! reads. Without the variable the plugin endpoint stays off, so a local `imbhd` never touches
//! `/run/docker`. See `docs/DOCKER_LOG_DRIVER.md`.
//!
//! Self-observability is opt-in at build time: `cargo build -p imbh-server --features tracing`
//! installs a `tracing-subscriber` fmt layer that renders imbh's internal spans/events to stderr.
//! Filter with `RUST_LOG` (e.g. `RUST_LOG=imbh=debug`); it defaults to `info` when unset. The
//! default build carries no `tracing` dependency at all (ARCHITECTURE.md §11 footprint gate).

use std::error::Error;
use std::thread::JoinHandle;

fn main() -> Result<(), Box<dyn Error>> {
    // Render imbh's internal instrumentation to stderr via the facade's `console` collector: it is
    // RUST_LOG-aware and, absent RUST_LOG, defaults every imbh target to `info` so the startup banner
    // and request spans show without extra setup.
    #[cfg(feature = "tracing")]
    imbh::console::init();

    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "./imbh-data".to_owned());
    // Positional argument first, then the environment, then the default; empty means "do not
    // listen". See `imbh_server::listen_addr` for why the environment has to be in the picture.
    let addr = imbh_server::listen_addr(
        args.next(),
        std::env::var("IMBH_LISTEN_ADDR").ok(),
        "127.0.0.1:4318",
    );
    #[cfg(feature = "grpc")]
    let grpc_addr = imbh_server::listen_addr(
        args.next(),
        std::env::var("IMBH_GRPC_LISTEN_ADDR").ok(),
        "127.0.0.1:4317",
    );

    // The flush scheduler: what turns buffered rows into Parquet segments (and lets the WAL be
    // reclaimed) without waiting for an operator to POST /admin/flush. A malformed spec is fatal —
    // silently falling back to a different flush cadence than the deployment asked for is worse than
    // refusing to start.
    let flush = imbh_server::flush_policy(std::env::var("IMBH_FLUSH").ok())?;
    let maintenance_interval =
        imbh_server::maintenance_interval(std::env::var("IMBH_MAINTENANCE_INTERVAL").ok())?;

    let db = imbh::Db::builder(&dir)
        .maintenance(imbh::Maintenance::Background(maintenance_interval))
        .flush(flush)
        .open()?;

    banner(&dir, addr.as_deref(), &flush, maintenance_interval);

    // Every configured endpoint runs on its own thread and `main` parks on all of them. The uniform
    // shape is what makes the listeners independently optional: the process stays alive as long as
    // anything is serving, whether that is HTTP, gRPC, the Docker plugin socket, or a subset.
    let mut servers: Vec<JoinHandle<()>> = Vec::new();

    // The Docker logging-driver plugin endpoint, when asked for. A bind failure is fatal — Docker
    // would otherwise mark the plugin healthy and every `docker run --log-driver imbh` would hang on
    // a socket nobody is listening to.
    #[cfg(all(feature = "docker", unix))]
    if let Some(sock) = std::env::var_os("IMBH_DOCKER_PLUGIN_SOCKET")
        .map(std::path::PathBuf::from)
        .filter(|s| !s.as_os_str().is_empty())
    {
        let plugin_db = db.clone();
        #[cfg(feature = "tracing")]
        tracing::info!(socket = %sock.display(), "docker log-driver plugin listening");
        #[cfg(not(feature = "tracing"))]
        println!("  docker:    {} (log-driver plugin)", sock.display());
        servers.push(std::thread::spawn(move || {
            if let Err(e) = imbh_server::docker::serve_plugin(plugin_db, &sock) {
                fatal(&format!("docker plugin error on {}: {e}", sock.display()));
            }
        }));
    }

    if let Some(addr) = addr {
        let http_db = db.clone();
        servers.push(std::thread::spawn(move || {
            if let Err(e) = imbh_server::serve(http_db, &addr) {
                fatal(&format!("HTTP server error on {addr}: {e}"));
            }
        }));
    }

    #[cfg(feature = "grpc")]
    if let Some(grpc_addr) = grpc_addr {
        let grpc_db = db.clone();
        #[cfg(feature = "tracing")]
        tracing::info!(%grpc_addr, "OTLP/gRPC: Logs/Trace/Metrics Service Export");
        #[cfg(not(feature = "tracing"))]
        println!("  OTLP/gRPC: {grpc_addr}  (Logs/Trace/Metrics Service Export)");
        servers.push(std::thread::spawn(move || {
            if let Err(e) = imbh_server::grpc::serve_grpc_blocking(grpc_db, &grpc_addr) {
                fatal(&format!("gRPC server error on {grpc_addr}: {e}"));
            }
        }));
    }

    if servers.is_empty() {
        return Err(
            "nothing to serve: every listener is disabled and no plugin socket is set".into(),
        );
    }
    for server in servers {
        let _ = server.join();
    }
    Ok(())
}

/// A listener died. Accept loops only return on a bind/serve error, and a half-serving `imbhd` is
/// worse than a dead one — a supervisor (or Docker, for the plugin) should restart it.
fn fatal(message: &str) -> ! {
    eprintln!("imbhd: {message}");
    std::process::exit(1);
}

/// The startup banner. With `tracing` on it flows through the subscriber as structured events;
/// otherwise the default build prints to stdout so `imbhd` stays self-describing.
fn banner(
    dir: &str,
    addr: Option<&str>,
    flush: &imbh::FlushPolicy,
    maintenance_interval: std::time::Duration,
) {
    #[cfg(feature = "tracing")]
    {
        match addr {
            Some(addr) => {
                tracing::info!(%addr, %dir, "imbhd listening");
                tracing::info!("OTLP/HTTP: POST /v1/logs, /v1/traces, /v1/metrics");
                tracing::info!("query: POST /api/query (SQL body -> JSON)");
            }
            None => tracing::info!(%dir, "imbhd started with no HTTP listener"),
        }
        tracing::info!(
            policy = %flush,
            retention_interval_secs = maintenance_interval.as_secs(),
            "flush scheduler"
        );
    }
    #[cfg(not(feature = "tracing"))]
    {
        match addr {
            Some(addr) => {
                println!("imbhd listening on http://{addr}  (data dir: {dir})");
                println!("  OTLP/HTTP: POST /v1/logs · /v1/traces · /v1/metrics");
                println!("  query:     POST /api/query  (SQL body → JSON)");
            }
            None => println!("imbhd started, no HTTP listener  (data dir: {dir})"),
        }
        // The effective policy, in the same syntax IMBH_FLUSH accepts — so an operator can copy it
        // back out, tweak one trigger, and know exactly what is running.
        println!(
            "  flush:     {flush}  (retention every {}s)",
            maintenance_interval.as_secs()
        );
    }
}
