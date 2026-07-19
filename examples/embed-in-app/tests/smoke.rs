//! Smoke test: the tri-signal host-app example runs end-to-end and exits 0.

#[test]
fn embed_in_app_runs() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_embed-in-app"))
        .output()
        .expect("run embed-in-app");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("buffered rows per table"),
        "stdout: {stdout}"
    );
}
