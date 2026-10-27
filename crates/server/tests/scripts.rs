//! The Server's scripts, run for real: `sh`, `bash`, `setsid`, `timeout`
//! and a real `python3 -m http.server`, on this machine as on a guest.
//!
//! `process.rs` proves what the Server sends; this proves it works. Linux
//! only, like the `process` runtime.
#![cfg(target_os = "linux")]

use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use beatbox_server::process::{ALIVE, CHECK_SECONDS, HTTP_CHECK, START, STOP, TCP_CHECK};

fn sh(script: &str, parameters: &[&str]) -> (i32, String) {
    let out = Command::new("/bin/sh")
        .args(["-c", script, "sh"])
        .args(parameters)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn check(script: &str, parameters: &[&str]) -> bool {
    Command::new("timeout")
        .args([CHECK_SECONDS, "bash", "-c", script, "bash"])
        .args(parameters)
        .status()
        .unwrap()
        .success()
}

fn have(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .is_ok_and(|out| out.status.success())
}

#[test]
fn start_check_and_stop_a_real_web_server() {
    if !["python3", "setsid", "timeout", "bash"]
        .iter()
        .all(|p| have(p))
    {
        eprintln!("skipped: needs python3, setsid, timeout and bash");
        return;
    }
    let room = std::env::temp_dir().join(format!("bbx-scripts-{}", std::process::id()));
    std::fs::create_dir_all(room.join("www")).unwrap();
    std::fs::write(room.join("www/index.html"), "<h1>hello</h1>").unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    let logs = room.join("logs");
    let www = room.join("www");

    let (code, said) = sh(
        START,
        &[
            logs.to_str().unwrap(),
            "site",
            "/",
            "python3",
            "-m",
            "http.server",
            &port,
            "--bind",
            "127.0.0.1",
            "--directory",
            www.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{said}");
    let pid = said.trim().to_owned();
    assert!(pid.parse::<u32>().is_ok(), "a pid: {said:?}");

    assert_eq!(sh(ALIVE, &[&pid, "python3"]).0, 0, "alive, and python3");
    assert_ne!(sh(ALIVE, &[&pid, "bash"]).0, 0, "not another program");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !check(TCP_CHECK, &[&port]) {
        assert!(Instant::now() < deadline, "it never listened");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(check(HTTP_CHECK, &[&port, "/"]), "GET / is a 200");
    assert!(
        !check(HTTP_CHECK, &[&port, "/nowhere"]),
        "a 404 is not well"
    );

    assert_eq!(sh(STOP, &[&pid]).0, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while sh(ALIVE, &[&pid, "python3"]).0 == 0 {
        assert!(Instant::now() < deadline, "it never stopped");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!check(TCP_CHECK, &[&port]), "nothing listens any more");
    let log = std::fs::read_to_string(logs.join("site.log")).unwrap();
    assert!(
        log.contains("GET / "),
        "its output went to its log: {log:?}"
    );
    // Stopping what is already gone is not an error.
    assert_eq!(sh(STOP, &[&pid]).0, 0);
    let _ = std::fs::remove_dir_all(room);
}
