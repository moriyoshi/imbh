//! Smoke test: the example binary runs end-to-end (built-in sample → tempdir → seal → SQL) and
//! exits 0. Uses `CARGO_BIN_EXE_*`, which Cargo sets for this crate's own binary.

#[test]
fn replay_otlp_file_runs() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_replay-otlp-file"))
        .output()
        .expect("run replay-otlp-file");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("accepted"), "stdout: {stdout}");
    assert!(stdout.contains("sealed"), "stdout: {stdout}");
}
