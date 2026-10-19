//! Opt-in end-to-end test of the **packaged** Docker logging-driver plugin against a **real daemon**
//! (Linux only).
//!
//! Every other plugin test stops short of the daemon: `docker_plugin_config.rs` checks the shipped
//! `config.json` statically and `docker_plugin_e2e.rs` drives the plugin API over a socket the test
//! binds itself. Neither can see the class of bug where the config is valid and the binary is valid
//! but `docker plugin enable` fails, or the daemon never routes a container's output to the plugin.
//! This test walks the whole packaging path instead:
//!
//! 1. `docker-plugin/build.sh` — compile `imbhd`, build and smoke-test the rootfs, `plugin create`
//!    under a unique throwaway name (with `IMBH_BIND=none`, so it opens no TCP port that could
//!    collide with an installed copy, and a `PLUGIN_STAMP` so its rootfs digest cannot collide
//!    with one either);
//! 2. `docker plugin enable`;
//! 3. a short-lived container with `--log-driver` set to that plugin, printing to stdout and stderr;
//! 4. `docker logs` on it — which the daemon answers through the plugin's `ReadLogs` (the plugin
//!    advertises the capability, so no dual-logging cache sits in between) — must return every
//!    line, on the stream it was written to. With `docker-remap` in the build (the default
//!    feature set), a JSON line must come back re-rendered as logfmt, which only the database
//!    could have produced: proof the lines made the round trip through imbh, not around it.
//!
//! The plugin and the container are removed afterwards whether the test passes or fails. The
//! `imbh-log-driver-rootfs` image `build.sh` tags is left in place (it is the same tag a manual
//! `build.sh` run produces).
//!
//! **Kept out of the default `cargo test --workspace` path twice over**, like `soak_rss.rs`: the test
//! is `#[ignore]`d, and even under `--ignored` it returns immediately (passing) unless
//! [`OPT_IN_ENV`] is `1` — so a blanket `--ignored` run (the soak workflow's idiom) never touches the
//! host's daemon by accident. Run it explicitly:
//!
//! ```sh
//! IMBH_DOCKER_DAEMON_E2E=1 cargo test -p imbh-server --test docker_plugin_daemon \
//!     -- --ignored --nocapture
//! ```
//!
//! Requirements and knobs (all environment variables):
//!
//! * a running daemon the invoking user may manage plugins on (`DOCKER="sudo docker"` for a
//!   rootful daemon from a non-root user — passed straight to `build.sh`, which honours it too);
//! * `THIRD-PARTY-NOTICES.txt` at the repo root (`build.sh` refuses without it);
//! * [`IMAGE_ENV`] (default `busybox:latest`): the container image, which must **already be
//!   local** — the container runs with `--pull=never`, so the test itself never reaches a registry
//!   (`build.sh`'s `docker build` may still pull its `BASE` image if that is not local either);
//! * [`TARGET_DIR_ENV`]: where the nested release build of `imbhd` goes. Defaults to a directory
//!   under cargo's per-test tmpdir, so it can never contend for the lock of the `cargo test` running
//!   it; point it at the workspace `target` to reuse an existing release build;
//! * `BASE`, `FEATURES`: forwarded to `build.sh` unchanged (`BASE=debian:trixie-slim` when this host's
//!   glibc is newer than bookworm's — `build.sh`'s smoke test says so when it is).
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Must be `1` for the test to touch the daemon at all.
const OPT_IN_ENV: &str = "IMBH_DOCKER_DAEMON_E2E";
/// The image the logging container runs. Must be local.
const IMAGE_ENV: &str = "IMBH_DOCKER_E2E_IMAGE";
const DEFAULT_IMAGE: &str = "busybox:latest";
/// Overrides the target directory of the nested `imbhd` release build.
const TARGET_DIR_ENV: &str = "IMBH_DOCKER_E2E_TARGET_DIR";

/// Longest `docker logs` gets to show every line once the container has exited. The plugin's ingest
/// worker flushes every 200 ms, so this is generous slack for a loaded machine.
const SETTLE: Duration = Duration::from_secs(30);

/// The container engine CLI, split the way `build.sh` splits `$DOCKER` (so `sudo docker` works).
fn engine() -> Vec<String> {
    let raw = std::env::var("DOCKER").unwrap_or_else(|_| "docker".to_owned());
    let words: Vec<String> = raw.split_whitespace().map(str::to_owned).collect();
    assert!(!words.is_empty(), "DOCKER is set but empty");
    words
}

fn docker(args: &[&str]) -> Command {
    let engine = engine();
    let mut cmd = Command::new(&engine[0]);
    cmd.args(&engine[1..]).args(args);
    cmd
}

/// Run a CLI call to completion, capturing both streams.
fn run(args: &[&str]) -> Output {
    docker(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn `{} {}`: {e}", engine().join(" "), args.join(" ")))
}

/// Run a CLI call that must succeed.
fn must(args: &[&str]) -> Output {
    let out = run(args);
    assert!(
        out.status.success(),
        "`{} {}` failed ({}):\n--- stdout\n{}\n--- stderr\n{}",
        engine().join(" "),
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

/// Removes the container and the plugin on drop — on success, on an assertion failure, and on a
/// panic anywhere in between (a test failure unwinds through it). Every step is best-effort and
/// silent: a resource that was never created is not an error.
struct Cleanup {
    plugin: String,
    container: String,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        // The container first: a plugin still referenced by a container refuses to be removed.
        let _ = run(&["rm", "-f", &self.container]);
        let _ = run(&["plugin", "disable", "-f", &self.plugin]);
        let rm = run(&["plugin", "rm", "-f", &self.plugin]);
        if !rm.status.success() && plugin_exists(&self.plugin) {
            eprintln!(
                "docker_plugin_daemon: could not remove plugin {}: {} -- remove it by hand",
                self.plugin,
                String::from_utf8_lossy(&rm.stderr).trim()
            );
        }
    }
}

fn plugin_exists(plugin: &str) -> bool {
    run(&["plugin", "inspect", plugin]).status.success()
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Run `build.sh` with its output streamed to the terminal (it is long, and `--nocapture` is the
/// documented invocation), returning only whether it succeeded.
fn build_plugin(plugin: &str, stamp: &str, target_dir: &Path) {
    let script = manifest_dir().join("docker-plugin/build.sh");
    let status = Command::new("sh")
        .arg(&script)
        .env("PLUGIN", plugin)
        .env("PLUGIN_STAMP", stamp)
        .env("IMBH_BIND", "none")
        .env("CARGO_TARGET_DIR", target_dir)
        .stdin(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", script.display()));
    assert!(
        status.success(),
        "{} failed ({status}); its output is above",
        script.display()
    );
}

/// Whether the plugin's `imbhd` was built with `docker-remap` — `build.sh`'s default feature set
/// does, a `FEATURES=` override may not.
fn remap_enabled() -> bool {
    std::env::var("FEATURES").map_or(true, |f| f.split(',').any(|f| f.trim() == "docker-remap"))
}

#[test]
#[ignore = "drives the host's Docker daemon: run with IMBH_DOCKER_DAEMON_E2E=1 and --ignored"]
fn packaged_plugin_logs_a_real_container_end_to_end() {
    if std::env::var(OPT_IN_ENV).as_deref() != Ok("1") {
        eprintln!("docker_plugin_daemon: {OPT_IN_ENV} is not 1 -- skipping (nothing touched)");
        return;
    }

    let image = std::env::var(IMAGE_ENV).unwrap_or_else(|_| DEFAULT_IMAGE.to_owned());
    // Fail fast and legibly, before the multi-minute build, on the two missing prerequisites that
    // would otherwise surface late and cryptically.
    must(&["version"]);
    assert!(
        run(&["image", "inspect", &image]).status.success(),
        "image {image} is not local; pull it first (`docker pull {image}`) or set {IMAGE_ENV} to \
         one that is -- the container runs with --pull=never"
    );

    let unique = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos()
    );
    let plugin = format!("imbh-e2e/log-driver:t{unique}");
    let container = format!("imbh-e2e-{unique}");
    // Armed before anything is created, so a `build.sh` that dies after `plugin create` is cleaned
    // up too.
    let _cleanup = Cleanup {
        plugin: plugin.clone(),
        container: container.clone(),
    };

    let target_dir = std::env::var_os(TARGET_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("docker-plugin-e2e"));
    build_plugin(&plugin, &unique, &target_dir);

    let enable = run(&["plugin", "enable", &plugin]);
    assert!(
        enable.status.success(),
        "`plugin enable {plugin}` failed: {}\nThe real reason is in the Docker daemon's log \
         (`journalctl -u docker`); there is no `docker plugin logs`.",
        String::from_utf8_lossy(&enable.stderr).trim()
    );

    // Distinct per run, so nothing but this run's container can satisfy the assertions.
    let nonce = format!("n{}", unique.replace('-', "x"));
    let out_1 = format!("imbh plugin e2e first stdout line {nonce}");
    let err_1 = format!("imbh plugin e2e stderr line {nonce}");
    let out_2 = format!("imbh plugin e2e last stdout line {nonce}");
    let json = format!(r#"{{"msg":"structured","nonce":"{nonce}"}}"#);
    let script = format!(
        "echo '{out_1}'; echo '{err_1}' >&2; echo '{json}'; echo '{out_2}'",
        json = json.replace('\'', r"'\''"),
    );
    must(&[
        "run",
        "--name",
        &container,
        "--pull=never",
        "--log-driver",
        &plugin,
        "--log-opt",
        "imbh-service=imbh-e2e",
        &image,
        "sh",
        "-c",
        &script,
    ]);

    // `docker logs` keeps the streams apart, so this also checks `log.iostream` survived the round
    // trip: a stderr line coming back on stdout would mean every line's stream was lost.
    let want_stdout = [out_1.as_str(), out_2.as_str()];
    let deadline = Instant::now() + SETTLE;
    let (stdout, stderr) = loop {
        let logs = run(&["logs", &container]);
        let stdout = String::from_utf8_lossy(&logs.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&logs.stderr).into_owned();
        let complete = logs.status.success()
            && want_stdout
                .iter()
                .all(|l| stdout.lines().any(|got| got == *l))
            && stderr.lines().any(|got| got == err_1)
            && stdout.lines().filter(|l| l.contains(&nonce)).count() == 3;
        if complete || Instant::now() >= deadline {
            break (stdout, stderr);
        }
        std::thread::sleep(Duration::from_millis(250));
    };

    for line in want_stdout {
        assert!(
            stdout.lines().any(|got| got == line),
            "`docker logs` stdout lacks {line:?}\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
        );
    }
    assert!(
        stderr.lines().any(|got| got == err_1),
        "`docker logs` stderr lacks {err_1:?}\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
    );
    assert!(
        !stdout.contains(&err_1),
        "the stderr line came back on stdout: log.iostream was lost\n{stdout}"
    );

    let structured = stdout
        .lines()
        .find(|l| l.contains(&nonce) && !want_stdout.contains(l))
        .unwrap_or_else(|| panic!("the JSON line did not come back\n{stdout}"));
    if remap_enabled() {
        // The built-in remap script stores a JSON line as a structured body, and ReadLogs renders
        // a structured body back as logfmt. A JSON-shaped line here would mean `docker logs` was
        // not answered from the database.
        assert!(
            !structured.starts_with('{')
                && structured.contains("msg=structured")
                && structured.contains(&format!("nonce={nonce}")),
            "expected the JSON line re-rendered as logfmt by the database, got {structured:?}"
        );
    } else {
        assert_eq!(
            structured, json,
            "without docker-remap a line is stored verbatim"
        );
    }
}
