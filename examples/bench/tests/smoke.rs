//! Smoke test: the bench harness runs a tiny workload (one OTLP body) and exits 0. Keeps it fast in
//! the debug test profile by passing a small record count instead of the 100k default.

#[test]
fn bench_runs_small() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bench"))
        .arg("200") // one 200-record body — enough to exercise ingest→seal→query
        .output()
        .expect("run bench");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("queries over"), "stdout: {stdout}");
}
