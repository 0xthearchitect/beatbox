//! Real PostgreSQL authentication on Unix when its programs are available.
#![cfg(unix)]
use std::process::Command;

#[test]
fn real_database_login_probe() {
    if Command::new("python3").arg("--version").output().is_err() {
        return;
    }
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/postgresql_probe_test.py");
    let done = Command::new("python3").arg(source).output().unwrap();
    assert!(
        done.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&done.stdout),
        String::from_utf8_lossy(&done.stderr)
    );
}
