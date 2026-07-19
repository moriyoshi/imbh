//! Smoke test: the RSS probe runs a tiny workload and prints its machine-readable summary line.
//! Linux-only (the probe reads `/proc/self/status`); a no-op elsewhere.

#[cfg(target_os = "linux")]
#[test]
fn rss_probe_runs_small() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rss-probe"))
        .arg("200") // one 200-record body — fast, still enough to move RSS
        .output()
        .expect("run rss-probe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("RSS_PROBE"), "stdout: {stdout}");
}
