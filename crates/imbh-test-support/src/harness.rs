//! A generalized re-exec harness for multi-process integration tests, distilled from the original
//! `imbh/tests/cross_process.rs`. A single `#[test]` decides its role by reading a role env var:
//! the parent spawns a copy of the test binary via `--exact <name>` with the role set, so the child
//! runs a *different* branch of the same test as a separate OS process. Coordination is via sentinel
//! files (no timing assumptions). This module owns the plumbing; each test composes the roles.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Build a `Command` that re-execs the current test binary, filtered to the single test `test_name`.
/// The caller adds `.env(role_env, role)` and any other env, then `.spawn()`. `--test-threads=1`
/// keeps libtest from running siblings in the child; `--nocapture` surfaces child output on failure.
pub fn child_command(test_name: &str) -> Command {
    let exe = std::env::current_exe().expect("current test executable path");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", test_name, "--nocapture", "--test-threads=1"]);
    cmd
}

/// Spawn a re-exec of `test_name` with `role_env=role` and `dir_env=<dir>` set. Convenience wrapper
/// over [`child_command`] for the common "one shared dir, one role var" shape.
pub fn spawn_role(test_name: &str, role_env: &str, role: &str, dir_env: &str, dir: &Path) -> Child {
    child_command(test_name)
        .env(role_env, role)
        .env(dir_env, dir)
        .spawn()
        .expect("spawn re-exec child")
}

/// The value of `role_env`, if set — the child branch selector. `None` means "this is the parent".
pub fn role(role_env: &str) -> Option<String> {
    std::env::var(role_env).ok()
}

/// Create (or truncate) a zero-byte sentinel file at `path`.
pub fn touch(path: &Path) {
    std::fs::write(path, b"").expect("write sentinel");
}

/// Block until `path` exists or `timeout` elapses; returns whether it appeared. Polls at 10 ms.
pub fn wait_for(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    path.exists()
}

/// A sentinel path `<dir>/<name>`.
pub fn sentinel(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}
