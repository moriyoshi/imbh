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
//! `IMBH_DUPLICATES` decides what happens to two metric datapoints sharing a series **and** a
//! timestamp, which has no PromQL meaning (issue #27). The default `error_on_read` accepts them at
//! ingest and fails a PromQL query that finds them, naming the metric, the label set and the instant.
//! `last_wins` collapses the duplicated instant at read time instead, so one bad point degrades one
//! datapoint rather than the whole metric — the escape hatch for a database that already holds
//! duplicates. `reject[,recent=N]` drops the repeat at ingest and reports it in the ingest response's
//! `rejected` count (and in OTLP/gRPC `partial_success`), so the responsible producer sees it at write
//! time; `recent` (default 262144) bounds both the guard's lookback and its memory, costing a fixed
//! ~13 MB. Rejecting is opt-in on purpose: dropping a producer's data should never happen by accident.
//!
//! `POST /mcp` serves the **Model Context Protocol** over its Streamable HTTP transport, so an agent
//! can search logs, pull traces, and query metrics through the same process that ingests them. The
//! tools are read-only; `IMBH_MCP_ALLOWED_ORIGINS` (comma-separated, or `*`) widens the default
//! loopback-only `Origin` check that guards against DNS rebinding. See `docs/MCP.md`.
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
//!
//! `imbhd` shuts down **gracefully** on `SIGINT`/`SIGTERM` (`Ctrl-C`, `docker stop`, systemd, `kill`):
//! every listener stops accepting, in-flight requests get `IMBH_SHUTDOWN_TIMEOUT` (default `5s`) to
//! finish, the Docker plugin's queued container lines are drained into the DB, and `Db::close()` seals
//! the buffer — so the next start replays nothing and the exit code is 0. A **second** signal exits
//! immediately with `128 + signum`. See `imbh_server::shutdown`.

use std::error::Error;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use imbh_server::Shutdown;

mod net;
use net::Net;

/// Extra patience beyond the drain each listener performs itself, covering the wake-up connection and
/// the plugin's ingest drain. A listener past this is not coming back before the process exits.
const STOP_GRACE: Duration = Duration::from_secs(2);

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
    let drain = imbh_server::shutdown_timeout(std::env::var("IMBH_SHUTDOWN_TIMEOUT").ok())?;
    // What happens to two metric points sharing a series and a timestamp (issue #27). Fatal on a
    // typo for the same reason as the flush spec: quietly running a different policy than the
    // deployment asked for either drops data it wanted or keeps data it wanted rejected.
    let duplicates = imbh_server::duplicates(std::env::var("IMBH_DUPLICATES").ok())?;
    // What bounds one connection: how long it may go quiet in each phase, how large a body it may
    // send, and how many of them may be open at once. `0` disables any of them individually.
    let limits = imbh_server::Limits {
        timeouts: imbh_server::io_timeouts(
            std::env::var("IMBH_HEADER_TIMEOUT").ok(),
            std::env::var("IMBH_BODY_TIMEOUT").ok(),
        )?,
        max_body: imbh_server::max_body(std::env::var("IMBH_MAX_BODY").ok())?,
        max_connections: imbh_server::max_connections(std::env::var("IMBH_MAX_CONNECTIONS").ok())?,
    };

    // The token every endpoint watches. Installed before anything is served, so a signal arriving
    // during startup is honoured by the listeners that come up after it rather than lost.
    let shutdown = Shutdown::with_drain_timeout(drain);
    if let Err(e) = shutdown.install_signal_handlers() {
        // Serving without it is still useful (a supervisor's SIGKILL plus WAL replay is a correct, if
        // slower, path), so this is a warning rather than a startup failure.
        warn(&format!(
            "no signal-driven shutdown: {e} — the buffer will be sealed by WAL replay instead"
        ));
    }

    // Runtime bridge-network discovery, and the access rule resolved against it. Started before the
    // listeners so `IMBH_LISTEN_ADDR=auto` has an answer by the time one binds; a no-op struct
    // without the `docker` feature, which is also the only build with anything to discover.
    let net = Arc::new(Net::new(&shutdown)?);

    let db = imbh::Db::builder(&dir)
        .maintenance(imbh::Maintenance::Background(maintenance_interval))
        .flush(flush)
        .duplicates(duplicates)
        .open()?;

    let shown = addr
        .as_deref()
        .map(|addr| net.describe_addr(addr, net::HTTP_PORT));
    let allow_from = net.describe_access();
    banner(
        &dir,
        Listening {
            addr: shown.as_deref(),
            allow_from: allow_from.as_deref(),
        },
        &flush,
        maintenance_interval,
        drain,
        limits,
        duplicates,
    );

    // Every configured endpoint runs on its own thread and `main` parks until shutdown. The uniform
    // shape is what makes the listeners independently optional: the process stays alive as long as
    // anything is serving, whether that is HTTP, gRPC, the Docker plugin socket, or a subset.
    //
    // Each thread reports its name when its accept loop has stopped *and* drained. `main` waits for
    // those reports instead of joining, so one wedged listener (a client that opened a socket and went
    // quiet) cannot hold up the final seal — it dies with the process a moment later.
    let (stopped_tx, stopped_rx) = std::sync::mpsc::channel::<&'static str>();
    let mut endpoints = 0usize;

    // The Docker logging-driver plugin endpoint, when asked for. A bind failure is fatal — Docker
    // would otherwise mark the plugin healthy and every `docker run --log-driver imbh` would hang on
    // a socket nobody is listening to.
    #[cfg(all(feature = "docker", unix))]
    if let Some(sock) = std::env::var_os("IMBH_DOCKER_PLUGIN_SOCKET")
        .map(std::path::PathBuf::from)
        .filter(|s| !s.as_os_str().is_empty())
    {
        let plugin_db = db.clone();
        // `IMBH_DOCKER_REMAP` picks the daemon-wide remap script; `--log-opt imbh-remap` overrides it
        // per container. Resolving here rather than inside the plugin keeps every environment knob in
        // one place (see the module doc), and it cannot fail — a bad script is reported per container.
        #[cfg_attr(not(feature = "docker-remap"), allow(unused_mut))]
        let mut plugin_config = imbh_server::docker::PluginConfig {
            // `container.network.*` resource attributes, when discovery can name the networks a
            // container is on. Only the Engine API can — the interface scan sees gateways and
            // subnets but cannot map a container to one — so these stay absent without it.
            networks: net
                .container_networks(std::env::var("IMBH_DOCKER_NETWORK_ATTRS").ok().as_deref())?,
            ..Default::default()
        };
        #[cfg(feature = "docker-remap")]
        {
            plugin_config.remap =
                imbh_server::docker_remap(std::env::var("IMBH_DOCKER_REMAP").ok())?;
        }
        #[cfg(feature = "tracing")]
        tracing::info!(socket = %sock.display(), "docker log-driver plugin listening");
        #[cfg(not(feature = "tracing"))]
        println!("  docker:    {} (log-driver plugin)", sock.display());
        endpoints += 1;
        serve_on_thread("docker plugin", &stopped_tx, {
            let shutdown = shutdown.clone();
            move || {
                imbh_server::docker::serve_plugin_with_config(
                    plugin_db,
                    &sock,
                    plugin_config,
                    shutdown,
                )
                .map_err(|e| format!("docker plugin error on {}: {e}", sock.display()))
            }
        });
    }

    if let Some(addr) = addr {
        let http_db = db.clone();
        endpoints += 1;
        serve_on_thread("HTTP", &stopped_tx, {
            let (shutdown, net) = (shutdown.clone(), Arc::clone(&net));
            move || net.serve_http(http_db, &addr, limits, shutdown)
        });
    }

    #[cfg(feature = "grpc")]
    if let Some(grpc_addr) = grpc_addr {
        let grpc_db = db.clone();
        #[cfg(feature = "tracing")]
        tracing::info!(grpc_addr = %net.describe_addr(&grpc_addr, net::GRPC_PORT),
            "OTLP/gRPC: Logs/Trace/Metrics Service Export");
        #[cfg(not(feature = "tracing"))]
        println!(
            "  OTLP/gRPC: {}  (Logs/Trace/Metrics Service Export)",
            net.describe_addr(&grpc_addr, net::GRPC_PORT)
        );
        endpoints += 1;
        serve_on_thread("OTLP/gRPC", &stopped_tx, {
            let (shutdown, net) = (shutdown.clone(), Arc::clone(&net));
            move || net.serve_grpc(grpc_db, &grpc_addr, shutdown)
        });
    }

    if endpoints == 0 {
        return Err(
            "nothing to serve: every listener is disabled and no plugin socket is set".into(),
        );
    }
    // Only the server threads keep a sender past this point, so the channel disconnects once they are
    // all gone — which is how `wait_for_endpoints` notices an early exit.
    drop(stopped_tx);

    // The life of the process: parked on the token until a signal (or a listener's fatal error, which
    // exits directly) ends it.
    let cause = shutdown.wait();
    report_stopping(cause, drain);
    wait_for_endpoints(&stopped_rx, endpoints, drain + STOP_GRACE);

    // The point of all of it: seal the buffer, drain the async-ingest queue, and join the maintenance
    // worker, so the rows accepted a moment ago are in Parquet rather than waiting for the next start
    // to replay the WAL. A failure here is worth an exit code — it means the data is only as durable
    // as the WAL made it.
    db.blocking().close()?;
    report_stopped();
    Ok(())
}

/// What the startup banner says about the listening posture: the HTTP address as it finally
/// resolved (`auto` becomes the gateways it found), and the peer filter when one is in force.
struct Listening<'a> {
    addr: Option<&'a str>,
    allow_from: Option<&'a str>,
}

/// Run one endpoint's blocking serve loop on its own thread, reporting to `stopped` when it returns.
/// A serve error is fatal (see [`fatal`]); a clean return means shutdown.
fn serve_on_thread(
    name: &'static str,
    stopped: &Sender<&'static str>,
    serve: impl FnOnce() -> Result<(), String> + Send + 'static,
) {
    let stopped = stopped.clone();
    std::thread::spawn(move || match serve() {
        Ok(()) => {
            let _ = stopped.send(name);
        }
        Err(message) => fatal(&message),
    });
}

/// Wait for `endpoints` listeners to report that they have stopped and drained, or for `timeout`.
/// Reports whoever is still going, because "the seal happened while an endpoint was still draining"
/// is the kind of thing an operator wants in the log rather than inferred from a truncated tail.
fn wait_for_endpoints(stopped: &Receiver<&'static str>, endpoints: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut left = endpoints;
    while left > 0 {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        match stopped.recv_timeout(remaining) {
            Ok(_name) => left -= 1,
            // Disconnected: every server thread is gone (each dropped its sender), so there is
            // nothing left to wait for.
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => break,
        }
    }
    if left > 0 {
        warn(&format!(
            "{left} endpoint(s) still draining after {timeout:?}; sealing anyway"
        ));
    }
}

/// A listener died. Accept loops only return an error on a bind/serve failure, and a half-serving
/// `imbhd` is worse than a dead one — a supervisor (or Docker, for the plugin) should restart it.
///
/// Deliberately skips the graceful path: the buffer is not sealed, because whatever broke the listener
/// is a poor moment to start writing Parquet, and the WAL already covers every accepted row — the next
/// start replays it.
fn fatal(message: &str) -> ! {
    eprintln!("imbhd: {message}");
    std::process::exit(1);
}

/// Report a startup or shutdown problem that is not fatal.
fn warn(message: &str) {
    #[cfg(feature = "tracing")]
    tracing::warn!("{message}");
    #[cfg(not(feature = "tracing"))]
    eprintln!("imbhd: {message}");
}

/// Announce that shutdown has begun, naming the signal that asked for it. A second signal skips
/// straight to exit, which is worth telling the operator who is waiting.
fn report_stopping(cause: Option<i32>, drain: Duration) {
    let by = cause.map_or("request", imbh_server::shutdown::signal_name);
    #[cfg(feature = "tracing")]
    tracing::info!(
        by,
        drain_secs = drain.as_secs_f64(),
        "shutting down: draining, then sealing (a second signal exits immediately)"
    );
    #[cfg(not(feature = "tracing"))]
    println!(
        "imbhd stopping ({by}): draining up to {drain:?}, then sealing  \
         (a second signal exits immediately)"
    );
}

/// Announce a completed graceful shutdown — the buffer is sealed and the DB is closed.
fn report_stopped() {
    #[cfg(feature = "tracing")]
    tracing::info!("shutdown complete: buffer sealed, database closed");
    #[cfg(not(feature = "tracing"))]
    println!("imbhd stopped: buffer sealed, database closed");
}

/// The startup banner. With `tracing` on it flows through the subscriber as structured events;
/// otherwise the default build prints to stdout so `imbhd` stays self-describing.
fn banner(
    dir: &str,
    listening: Listening<'_>,
    flush: &imbh::FlushPolicy,
    maintenance_interval: Duration,
    drain: Duration,
    limits: imbh_server::Limits,
    duplicates: imbh::Duplicates,
) {
    let Listening { addr, allow_from } = listening;
    #[cfg(feature = "tracing")]
    {
        match addr {
            Some(addr) => {
                tracing::info!(%addr, %dir, "imbhd listening");
                tracing::info!("OTLP/HTTP: POST /v1/logs, /v1/traces, /v1/metrics");
                tracing::info!("query: POST /api/query (SQL body -> JSON)");
                tracing::info!("mcp: POST /mcp (Model Context Protocol, read-only tools)");
            }
            None => tracing::info!(%dir, "imbhd started with no HTTP listener"),
        }
        if let Some(allow_from) = allow_from {
            tracing::info!(%allow_from, "connections are accepted only from these networks");
        }
        tracing::info!(
            policy = %flush,
            retention_interval_secs = maintenance_interval.as_secs(),
            shutdown_drain_secs = drain.as_secs_f64(),
            header_timeout_secs = limits.timeouts.header.as_secs_f64(),
            body_timeout_secs = limits.timeouts.body.as_secs_f64(),
            max_body_bytes = limits.max_body,
            max_connections = limits.max_connections,
            %duplicates,
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
                println!("  mcp:       POST /mcp  (Model Context Protocol, read-only tools)");
            }
            None => println!("imbhd started, no HTTP listener  (data dir: {dir})"),
        }
        if let Some(allow_from) = allow_from {
            println!("  allow:     {allow_from}  (every other peer is refused on accept)");
        }
        // The effective policy, in the same syntax IMBH_FLUSH accepts — so an operator can copy it
        // back out, tweak one trigger, and know exactly what is running.
        println!(
            "  flush:     {flush}  (retention every {}s)",
            maintenance_interval.as_secs()
        );
        println!("  shutdown:  SIGINT/SIGTERM → drain up to {drain:?}, then seal");
        // Zero means "no deadline", so say that rather than printing `0ns`.
        let show = |d: Duration| match d.is_zero() {
            true => "off".to_owned(),
            false => format!("{d:?}"),
        };
        println!(
            "  timeouts:  headers {} (total) · body {} (per read)",
            show(limits.timeouts.header),
            show(limits.timeouts.body)
        );
        // Zero means "no cap" for both of these, same as for the deadlines above.
        let cap = |n: u64| match n {
            0 => "off".to_owned(),
            n => n.to_string(),
        };
        println!(
            "  limits:    body {} bytes · {} connections",
            cap(limits.max_body),
            cap(limits.max_connections as u64)
        );
        // In the same syntax IMBH_DUPLICATES accepts, so an operator can copy it back out.
        println!("  metrics:   duplicate timestamps → {duplicates}");
    }
}
