//! Exercise version selection and config preservation with executable fixtures.
#![cfg(unix)]
use std::process::Command;

#[test]
fn service_preparation_preserves_existing_data_and_configuration() {
    if Command::new("python3").arg("--version").output().is_err() {
        return;
    }
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/preparation_test.py");
    let done = Command::new("python3").arg(source).output().unwrap();
    assert!(
        done.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&done.stdout),
        String::from_utf8_lossy(&done.stderr)
    );
}
