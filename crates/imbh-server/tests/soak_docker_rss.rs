//! Opt-in RSS soak for the **Docker logging-driver plugin** (Linux only), scaling by *container
//! count* — the axis the VRL remapper's cost actually rides on.
//!
//! `crates/imbh/tests/soak_rss.rs` measures the *database*. It says nothing about the log driver,
//! and nothing at all about `docker-remap`: every container's FIFO reader thread owns its own VRL
//! [`Runtime`](vrl) and re-seeds an event object per line, so the plausibly-affected axis is how many
//! containers are logging, not how fast they log. This file measures exactly that.
//!
//! **Shape.** One *cell* = one fresh process running the real plugin (`serve_plugin_with_config`)
//! over real FIFOs, with `containers` writer threads that keep their write ends **open** while RSS is
//! sampled — so the reader threads, and their remappers, are alive at the moment of measurement. A
//! cell reports `VmRSS` before any container starts (idle), `VmRSS` once every line has landed in the
//! database (steady), and `VmHWM` (peak), all from `/proc/self/status` — the same mechanism
//! `examples/rss-probe` and `soak_rss.rs` use, so the figures are directly comparable.
//!
//! The parent run spawns those cells by re-executing this test binary with [`CELL_ENV`] set
//! (the `imbh-test-support::harness` re-exec idiom), so every point on the ladder gets a clean
//! address space instead of inheriting the previous one's allocator state.
//!
//! **Differential.** Within a `docker-remap` build the matrix runs every cell in each [`Remap`]
//! column — `off`, the built-in script, and (on the line-rate series) the identity script `.` as a
//! control — and the number that matters is the *difference*, not the absolute. Built with plain
//! `docker` the crate has no `remap` knob at all, so only the `off` column exists; run it both ways
//! to also see the cost of merely *linking* VRL:
//!
//! ```sh
//! cargo test --release -p imbh-server --features docker-remap \
//!     --test soak_docker_rss -- --ignored --nocapture
//! cargo test --release -p imbh-server --features docker \
//!     --test soak_docker_rss -- --ignored --nocapture
//! ```
//!
//! Release matters: a debug-build RSS number is not comparable to OVERVIEW.md §2's budgets.
//!
//! Kept out of the default `cargo test --workspace` path twice over — the whole file is `#[cfg]`'d
//! on the off-by-default `docker` feature, and the one test in it is `#[ignore]`d — matching
//! `soak_rss.rs` and `.github/workflows/soak.yml`.
#![cfg(all(feature = "docker", unix, target_os = "linux"))]

use std::io::{BufWriter, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use imbh::Db;
use imbh_server::Shutdown;
use imbh_server::docker::entry::{LogEntry, write_entry};
use imbh_server::docker::{PluginConfig, serve_plugin_with_config};
use imbh_test_support::procinfo::vm_rss_bytes;

/// Set by the parent on a re-exec to say "you are one cell, here is your spec".
const CELL_ENV: &str = "IMBH_DOCKER_SOAK_CELL";
/// The one test in this file, so a cell can re-exec straight back into it.
const TEST_NAME: &str = "docker_plugin_rss_scales_with_container_count";

/// Longest a cell waits for its lines to travel FIFO → ingest worker → DB before giving up.
const SETTLE: Duration = Duration::from_secs(180);
/// Longest the parent waits on one cell.
const CELL_TIMEOUT: Duration = Duration::from_secs(600);

/// Lines per container on the container-count ladder. Enough to make every remapper actually run
/// the script many times; small enough that the 100-container cell stays a few seconds of ingest.
const LADDER_LINES: usize = 200;
/// Container counts on the ladder. `0` is the plugin's own idle floor.
const LADDER: [usize; 5] = [0, 1, 10, 50, 100];
/// Fixed container count for the line-rate series — the control for "cost does not scale with
/// lines".
const RATE_CONTAINERS: usize = 10;
/// Lines per container for that series, two decades apart.
const RATE_LINES: [usize; 3] = [100, 1_000, 10_000];

// OVERVIEW.md §2, as the budgets a *remapping plugin* has to hold to. Hard limits rather than
// targets: this process is the plugin **plus** the test's own writer threads and framing buffers, so
// it can only overstate, and a soak that trips on the target would be measuring the harness.
const IDLE_HARD_BYTES: u64 = 64 * 1024 * 1024;
const STEADY_HARD_BYTES: u64 = 320 * 1024 * 1024;

// ── one cell ─────────────────────────────────────────────────────────────────────────────

/// Which script, if any, the plugin runs over every line.
///
/// [`Remap::Identity`] is the control that makes the line-rate series interpretable. `.` provably
/// produces byte-identical records to no script at all (`remap.rs`'s
/// `an_identity_script_reproduces_the_unremapped_record`), and it *does* read the event root, so
/// `wants_info` is true and the full seed — container info and all — is cloned per line. Identity
/// minus off is therefore the remapper **machinery** alone: the `Runtime`, the seed clone, the
/// per-line allocation churn. Builtin minus identity is what the *parsed, structured* record costs
/// downstream, which is a payload-size effect and not the remapper's own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Remap {
    Off,
    Identity,
    Builtin,
}

impl Remap {
    fn as_str(self) -> &'static str {
        match self {
            Remap::Off => "off",
            Remap::Identity => "identity",
            Remap::Builtin => "builtin",
        }
    }

    fn parse(text: &str) -> Remap {
        match text {
            "identity" => Remap::Identity,
            "builtin" => Remap::Builtin,
            _ => Remap::Off,
        }
    }
}

/// One point of the matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    containers: usize,
    lines: usize,
    /// Which script the plugin runs. Always [`Remap::Off`] without `docker-remap`.
    remap: Remap,
}

impl Cell {
    fn spec(&self) -> String {
        format!("{}:{}:{}", self.containers, self.lines, self.remap.as_str())
    }

    fn parse(spec: &str) -> Cell {
        let mut parts = spec.split(':');
        let mut next = || parts.next().expect("cell spec has three fields");
        let containers = next().parse().expect("container count");
        let lines = next().parse().expect("line count");
        let remap = Remap::parse(next());
        Cell {
            containers,
            lines,
            remap,
        }
    }
}

/// What a cell measured.
#[derive(Clone, Copy, Debug)]
struct Sample {
    idle: u64,
    steady: u64,
    peak: u64,
    rows: i64,
    seconds: f64,
}

// ── the test ─────────────────────────────────────────────────────────────────────────────

/// Parent: run the matrix. Child (re-exec with [`CELL_ENV`]): be one cell.
#[test]
#[ignore = "Docker log-driver RSS soak: run explicitly with --ignored (see the module docs)"]
fn docker_plugin_rss_scales_with_container_count() {
    match std::env::var(CELL_ENV) {
        Ok(spec) => run_cell(Cell::parse(&spec)),
        Err(_) => run_matrix(),
    }
}

/// Columns of the container ladder: the differential that answers "what does remapping cost per
/// container?". Without the feature there is no `remap` knob on `PluginConfig` at all, so there is
/// only one column and the run measures the plain driver.
fn ladder_columns() -> &'static [Remap] {
    match cfg!(feature = "docker-remap") {
        true => &[Remap::Off, Remap::Builtin],
        false => &[Remap::Off],
    }
}

/// Columns of the line-rate series, with the identity control in the middle.
fn rate_columns() -> &'static [Remap] {
    match cfg!(feature = "docker-remap") {
        true => &[Remap::Off, Remap::Identity, Remap::Builtin],
        false => &[Remap::Off],
    }
}

fn run_matrix() {
    let build = match cfg!(feature = "docker-remap") {
        true => "docker,docker-remap",
        false => "docker",
    };
    let profile = match cfg!(debug_assertions) {
        true => "debug (NOT comparable to OVERVIEW.md §2 — rerun with --release)",
        false => "release",
    };
    println!("SOAK_DOCKER build={build} profile={profile}");

    // ── the container-count ladder ────────────────────────────────────────────────────────
    let mut ladder: Vec<(Cell, Option<Sample>)> = Vec::new();
    for &remap in ladder_columns() {
        for containers in LADDER {
            let cell = Cell {
                containers,
                lines: LADDER_LINES,
                remap,
            };
            ladder.push((cell, spawn_cell(cell)));
        }
    }
    if ladder.iter().all(|(_, s)| s.is_none()) {
        eprintln!("skipping: no cell produced a sample (mkfifo unavailable?)");
        return;
    }
    print_table(
        &format!("container ladder ({LADDER_LINES} lines/container)"),
        &ladder,
    );

    // ── the line-rate series, at a fixed container count ──────────────────────────────────
    let mut rate: Vec<(Cell, Option<Sample>)> = Vec::new();
    for &remap in rate_columns() {
        for lines in RATE_LINES {
            let cell = Cell {
                containers: RATE_CONTAINERS,
                lines,
                remap,
            };
            rate.push((cell, spawn_cell(cell)));
        }
    }
    print_table(
        &format!("line-rate series ({RATE_CONTAINERS} containers)"),
        &rate,
    );

    // ── what the numbers mean ─────────────────────────────────────────────────────────────
    let steady = |rows: &[(Cell, Option<Sample>)], want: Cell| -> Option<u64> {
        rows.iter()
            .find(|(cell, sample)| *cell == want && sample.is_some())
            .and_then(|(_, sample)| sample.map(|s| s.steady))
    };
    let at = |containers, remap| {
        steady(
            &ladder,
            Cell {
                containers,
                lines: LADDER_LINES,
                remap,
            },
        )
    };

    // Slope of the ladder: what one more logging container costs, in each column.
    for &remap in ladder_columns() {
        if let (Some(one), Some(many)) = (at(1, remap), at(100, remap)) {
            println!(
                "SOAK_DOCKER marginal remap={} per_container={:.3} MiB  (steady@100 {} MiB - steady@1 {} MiB, over 99 containers)",
                remap.as_str(),
                mib(many as f64 - one as f64) / 99.0,
                many >> 20,
                one >> 20
            );
        }
    }
    // The differential itself, per rung.
    for containers in LADDER {
        if let (Some(off), Some(on)) = (at(containers, Remap::Off), at(containers, Remap::Builtin))
        {
            let delta = on as f64 - off as f64;
            println!(
                "SOAK_DOCKER remap_cost containers={containers} delta={:.1} MiB per_container={}",
                mib(delta),
                match containers {
                    0 => "n/a".to_owned(),
                    n => format!("{:.3} MiB", mib(delta) / n as f64),
                }
            );
        }
    }
    // Does the cost ride on the line count? The identity column is the control: identity − off is
    // the remapper machinery, builtin − identity is the fatter structured record it produces.
    for lines in RATE_LINES {
        let cell = |remap| {
            steady(
                &rate,
                Cell {
                    containers: RATE_CONTAINERS,
                    lines,
                    remap,
                },
            )
        };
        if let (Some(off), Some(identity), Some(builtin)) = (
            cell(Remap::Off),
            cell(Remap::Identity),
            cell(Remap::Builtin),
        ) {
            println!(
                "SOAK_DOCKER line_rate containers={RATE_CONTAINERS} lines={lines} off={} MiB \
                 machinery(identity-off)={:+.1} MiB payload(builtin-identity)={:+.1} MiB",
                off >> 20,
                mib(identity as f64 - off as f64),
                mib(builtin as f64 - identity as f64),
            );
        }
    }

    // ── the budget gate ───────────────────────────────────────────────────────────────────
    // The worst cell of the run — the most containers, remapping where the build can.
    let worst = *ladder_columns().last().expect("at least one column");
    if let Some(idle) = ladder
        .iter()
        .find(|(c, s)| s.is_some() && c.containers == 0 && c.remap == worst)
        .and_then(|(_, s)| s.map(|s| s.idle))
    {
        assert!(
            idle < IDLE_HARD_BYTES,
            "idle RSS {} MiB exceeds OVERVIEW.md §2's {} MiB hard limit for a remapping plugin",
            idle >> 20,
            IDLE_HARD_BYTES >> 20
        );
    }
    let peak_steady = ladder
        .iter()
        .chain(rate.iter())
        .filter_map(|(_, s)| s.map(|s| s.steady))
        .max()
        .expect("at least one sample");
    assert!(
        peak_steady < STEADY_HARD_BYTES,
        "steady RSS {} MiB exceeds OVERVIEW.md §2's {} MiB hard limit for a remapping plugin",
        peak_steady >> 20,
        STEADY_HARD_BYTES >> 20
    );
}

fn mib(bytes: f64) -> f64 {
    bytes / (1024.0 * 1024.0)
}

fn print_table(title: &str, rows: &[(Cell, Option<Sample>)]) {
    println!("\n== {title} ==");
    println!(
        "{:>10}  {:>7}  {:>8}  {:>9}  {:>11}  {:>9}  {:>9}  {:>6}",
        "containers", "lines", "remap", "idle MiB", "steady MiB", "peak MiB", "rows", "secs"
    );
    for (cell, sample) in rows {
        match sample {
            None => println!(
                "{:>10}  {:>7}  {:>8}  {:>9}",
                cell.containers,
                cell.lines,
                cell.remap.as_str(),
                "skipped"
            ),
            Some(s) => println!(
                "{:>10}  {:>7}  {:>8}  {:>9}  {:>11}  {:>9}  {:>9}  {:>6.1}",
                cell.containers,
                cell.lines,
                cell.remap.as_str(),
                s.idle >> 20,
                s.steady >> 20,
                s.peak >> 20,
                s.rows,
                s.seconds
            ),
        }
    }
}

/// Re-execute this test binary as one cell and parse the line it prints.
fn spawn_cell(cell: Cell) -> Option<Sample> {
    let exe = std::env::current_exe().expect("the test binary's own path");
    let mut child = std::process::Command::new(exe)
        .args([TEST_NAME, "--exact", "--ignored", "--nocapture"])
        .env(CELL_ENV, cell.spec())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("re-exec the soak binary as a cell");

    // Read the child's stdout on this thread while it runs, then reap it. `wait_with_output` would
    // deadlock on a full pipe otherwise.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut text = String::new();
    stdout.read_to_string(&mut text).expect("read cell output");

    let deadline = Instant::now() + CELL_TIMEOUT;
    loop {
        match child.try_wait().expect("wait on the cell") {
            Some(status) => {
                assert!(status.success(), "cell {} failed:\n{text}", cell.spec());
                break;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!(
                    "cell {} did not finish within {CELL_TIMEOUT:?}",
                    cell.spec()
                );
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    if text.contains("SOAK_DOCKER_SKIP") {
        return None;
    }
    let line = text
        .lines()
        .find(|l| l.starts_with("SOAK_DOCKER_CELL "))
        .unwrap_or_else(|| panic!("cell {} printed no result:\n{text}", cell.spec()));
    let field = |key: &str| -> String {
        line.split_whitespace()
            .find_map(|f| f.strip_prefix(key)?.strip_prefix('=').map(str::to_owned))
            .unwrap_or_else(|| panic!("cell output has no {key}: {line}"))
    };
    let bytes = |key: &str| field(key).parse::<u64>().expect("a byte count") * 1024;
    Some(Sample {
        idle: bytes("idle_kb"),
        steady: bytes("steady_kb"),
        peak: bytes("peak_kb"),
        rows: field("rows").parse().expect("a row count"),
        seconds: field("secs").parse().expect("a duration"),
    })
}

// ── the cell body: a real plugin, real FIFOs, real containers ────────────────────────────

fn run_cell(cell: Cell) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let socket = tmp.path().join("imbh.sock");
    let db_dir = tmp.path().join("db");
    std::fs::create_dir_all(&db_dir).expect("create the db dir");
    // On disk rather than in memory: an in-memory `Db` would hold every row resident and swamp the
    // differential this soak exists to measure.
    let db: Arc<Db> = Db::builder(&db_dir).open().expect("open the db");

    let shutdown = Shutdown::new();
    let server = {
        let (db, socket, shutdown) = (db.clone(), socket.clone(), shutdown.clone());
        // `PluginConfig` grows the `remap` field only under `docker-remap`; without it the update is
        // trivially exhaustive.
        #[allow(clippy::needless_update)]
        let config = PluginConfig {
            #[cfg(feature = "docker-remap")]
            remap: match cell.remap {
                Remap::Off => imbh_server::docker::remap::Source::Off,
                // `.` — the event unchanged. Compiles to a program that queries the root, so the
                // seed (container info included) is built and cloned per line exactly as a real
                // script's would be, while the record it yields is the un-remapped one.
                Remap::Identity => imbh_server::docker::remap::Source::Inline(".".to_owned()),
                Remap::Builtin => imbh_server::docker::remap::Source::Builtin,
            },
            ..Default::default()
        };
        std::thread::spawn(move || {
            serve_plugin_with_config(db, &socket, config, shutdown).expect("serve the plugin")
        })
    };
    wait_for_socket(&socket);

    // Idle: the plugin is up, nothing is logging. Sampled before any FIFO exists so it is the
    // process floor, not a container cost.
    let idle = vm_rss_bytes().expect("VmRSS readable on Linux");

    let fifos: Vec<PathBuf> = (0..cell.containers)
        .map(|i| tmp.path().join(format!("fifo-{i}")))
        .collect();
    if !fifos.is_empty() && !mkfifo(&fifos) {
        println!("SOAK_DOCKER_SKIP mkfifo unavailable");
        shutdown.trigger();
        let _ = server.join();
        return;
    }

    let started = Instant::now();
    // Writers block on opening the write end until the plugin opens the read end, exactly as dockerd
    // does — so they are spawned first and `StartLogging` is what releases them.
    let hold = Arc::new(AtomicBool::new(true));
    let writers: Vec<_> = fifos
        .iter()
        .enumerate()
        .map(|(i, fifo)| {
            let (path, hold, lines) = (fifo.clone(), hold.clone(), cell.lines);
            std::thread::spawn(move || {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .expect("open the fifo for writing");
                let mut out = BufWriter::new(file);
                for n in 0..lines {
                    write_entry(&mut out, &line(i, n)).expect("frame a line");
                }
                out.flush().expect("flush the last frame");
                // THE POINT: hold the write end open. A reader thread — and the VRL `Runtime` it
                // owns — lives only as long as its container's stream, so releasing here would
                // measure a plugin with zero live containers.
                while hold.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        })
        .collect();

    for (i, fifo) in fifos.iter().enumerate() {
        let reply = post(
            &socket,
            "/LogDriver.StartLogging",
            &start_logging_body(fifo, i),
        );
        assert!(
            reply.contains(r#""Err":""#),
            "StartLogging for container {i} failed: {reply}"
        );
    }

    let want = (cell.containers * cell.lines) as i64;
    let rows = wait_for_rows(&db, want);
    let seconds = started.elapsed().as_secs_f64();

    // Steady: every line is in the database and every container is still logging.
    let steady = vm_rss_bytes().expect("VmRSS readable on Linux");
    let peak = vm_hwm_bytes().expect("VmHWM readable on Linux");
    println!(
        "SOAK_DOCKER_CELL containers={} lines={} remap={} idle_kb={} steady_kb={} peak_kb={} rows={rows} secs={seconds:.1}",
        cell.containers,
        cell.lines,
        cell.remap.as_str(),
        idle / 1024,
        steady / 1024,
        peak / 1024,
    );
    assert_eq!(rows, want, "not every container line reached the database");

    hold.store(false, Ordering::Relaxed);
    for writer in writers {
        writer.join().expect("writer thread");
    }
    shutdown.trigger();
    server.join().expect("the plugin accept loop returns");
}

/// A container line with enough structure that the built-in script does real parsing work — the
/// JSON tier, a level to lift onto the record, and a handful of typed fields.
fn line(container: usize, n: usize) -> LogEntry {
    let level = ["info", "warn", "error", "debug"][n % 4];
    let body = format!(
        r#"{{"level":"{level}","msg":"request completed","path":"/orders/{n}","status":200,"dur_ms":{},"container":{container}}}"#,
        n % 97
    );
    LogEntry {
        source: match n % 8 {
            7 => "stderr".to_owned(),
            _ => "stdout".to_owned(),
        },
        time_nano: 1_700_000_000_000_000_000 + (container * 1_000_000 + n) as i64,
        line: format!("{body}\n").into_bytes(),
        partial: false,
        partial_log_metadata: None,
    }
}

fn start_logging_body(fifo: &Path, i: usize) -> Vec<u8> {
    format!(
        r#"{{"File":{:?},"Info":{{"ContainerID":"soak{i:012}","ContainerName":"/soak-{i}",
            "ContainerImageName":"nginx:1.27","ContainerLabels":{{"app":"soak"}},
            "ContainerEnv":["REGION=eu-1"],"Config":{{}}}}}}"#,
        fifo.display().to_string()
    )
    .into_bytes()
}

fn mkfifo(paths: &[PathBuf]) -> bool {
    std::process::Command::new("mkfifo")
        .args(paths)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn wait_for_socket(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if UnixStream::connect(socket).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the plugin socket never came up at {}", socket.display());
}

/// Poll `count(*)` until it reaches `want` (or stops moving past the deadline).
///
/// The query runs at least once even for an empty cell, so DataFusion's planner and pool are
/// resident in *every* sample. Otherwise the 0-container cell would be the only one that never
/// touched the query engine, and the 0 → 1 step of the ladder would read as a per-container cost
/// when it is really the first `SELECT`.
fn wait_for_rows(db: &Arc<Db>, want: i64) -> i64 {
    let blocking = db.blocking();
    let deadline = Instant::now() + SETTLE;
    loop {
        let batches = blocking
            .sql("SELECT count(*) AS n FROM logs")
            .expect("count the ingested rows");
        let rows = batches
            .first()
            .map(|b| imbh_test_support::assert::int_at(b, 0))
            .unwrap_or(0);
        if rows >= want || Instant::now() >= deadline {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `POST path` to the plugin socket, returning the response body as text.
fn post(socket: &Path, path: &str, body: &[u8]) -> String {
    let mut stream = UnixStream::connect(socket).expect("connect to the plugin socket");
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: docker\r\nContent-Type: application/json\r\n\
         Connection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(body).expect("write body");
    stream.flush().expect("flush");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response has a header/body separator");
    String::from_utf8_lossy(&raw[split + 4..]).into_owned()
}

/// Peak resident set size (`VmHWM`) in bytes — the high-water mark since process start. Mirrors
/// `imbh-test-support`'s `vm_rss_bytes`, which reads `VmRSS` from the same file.
fn vm_hwm_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            // "VmHWM:  \t   12345 kB" — the value is kiB despite the `kB` label (kernel quirk).
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}
