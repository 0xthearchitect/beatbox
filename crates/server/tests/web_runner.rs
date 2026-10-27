//! Exercise the embedded runner against actual files, HTTP and subprocesses.
// The runner executes in Linux guests, even when BeatBox runs on Windows.
// These local integration cases require POSIX signals and process groups.
#![cfg(unix)]

use std::process::Command;

#[test]
fn website_runner_works_with_files_and_applications() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: web runner tests need python3");
        return;
    }
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/web_runner_test.py");
    let result = Command::new("python3").arg(tests).output().unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
