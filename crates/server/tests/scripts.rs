//! The Server's scripts, run for real: `sh`, `bash`, `setsid`, `timeout`
//! and a real `python3 -m http.server`, on this machine as on a guest.
//!
//! `process.rs` proves what the Server sends; this proves it works. Linux
//! only, like the `process` runtime.
#![cfg(target_os = "linux")]

use std::fs::Permissions;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

use beatbox_server::process::{
    ALIVE, APPEND, APPLY, CHECK_SECONDS, HTTP_CHECK, HTTP_SAID, INSTALL, LISTENING, PACK, PIECE,
    PLAN, PREPARING, SIZE, STAGE, START, STOP, SUM, TCP_CHECK, UNPACK, UNSTAGE,
};

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

/// `setsid` is slow to become the service here, as on a busy machine.
#[test]
fn a_start_that_is_slow_to_exec_is_still_alive_when_it_returns() {
    if !["setsid", "sleep"].iter().all(|p| have(p)) {
        eprintln!("skipped: needs setsid and sleep");
        return;
    }
    let room = std::env::temp_dir().join(format!("bbx-slow-exec-{}", std::process::id()));
    let bin = room.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let real = sh("command -v setsid", &[]).1;
    let wrapper = bin.join("setsid");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nsleep 0.3\nexec {} \"$@\"\n", real.trim()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Command::new("/bin/sh")
        .env("PATH", path)
        .args(["-c", START, "sh"])
        .args([room.to_str().unwrap(), "slow", "/", "sleep", "5"])
        .output()
        .unwrap();
    let pid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let alive = sh(ALIVE, &[&pid, "sleep"]).0;
    sh(STOP, &[&pid]);
    let _ = std::fs::remove_dir_all(&room);
    assert_eq!(alive, 0, "pid {pid:?} had not become the service yet");
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
    // Big enough that the site is still sending when the check has its answer.
    std::fs::write(room.join("www/big.html"), "x".repeat(1 << 20)).unwrap();
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
    for _ in 0..5 {
        assert!(
            check(HTTP_CHECK, &[&port, "/big.html"]),
            "a big page is well"
        );
    }
    let said = Command::new("timeout")
        .args([
            CHECK_SECONDS,
            "bash",
            "-c",
            HTTP_SAID,
            "bash",
            &port,
            "/index.html",
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&said.stdout),
        "code=200\n<h1>hello</h1>",
        "the status, then the body without its headers"
    );
    if have("ss") {
        let (code, listening) = sh(LISTENING, &[]);
        assert_eq!(code, 0);
        assert!(
            listening
                .lines()
                .any(|line| line == format!("{port} {pid}")),
            "its port, held by its pid: {listening}"
        );
        assert!(listening.contains("\n--\n"));
    }

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
    assert!(
        !log.contains("Traceback"),
        "a check cut the site off mid-answer: {log}"
    );
    // Stopping what is already gone is not an error.
    assert_eq!(sh(STOP, &[&pid]).0, 0);
    let _ = std::fs::remove_dir_all(room);
}

/// The preparation scripts install as root on a guest, so here they are
/// only read by `sh`, and the two that never change anything are run.
#[test]
fn the_preparation_scripts_parse_and_the_lookers_only_look() {
    for script in [PLAN, APPLY, INSTALL, PREPARING] {
        let parsed = Command::new("/bin/sh")
            .args(["-n", "-c", script])
            .status()
            .unwrap();
        assert!(parsed.success(), "sh cannot read:\n{script}");
    }
    let (code, said) = sh(PLAN, &["nginx"]);
    assert_eq!(code, 0, "{said}");
    assert!(said.starts_with("system="), "{said}");
    let (code, said) = sh(PREPARING, &[]);
    assert_eq!(code, 0, "{said}");
    assert!(said.starts_with("state="), "{said}");
}

#[test]
fn a_copy_packed_in_pieces_unpacks_over_what_changed() {
    for script in [STAGE, PACK, SUM, PIECE, APPEND, UNPACK, UNSTAGE, SIZE] {
        assert_eq!(sh(&format!("exit 0\n{script}"), &[]).0, 0, "{script}");
    }
    let definition: serde_json::Value =
        serde_json::from_str(include_str!("../definitions/postgresql.json")).unwrap();
    let dump = definition["data"]["dump"][2].as_str().unwrap();
    let parsed = Command::new("bash")
        .args(["-n", "-c", dump])
        .status()
        .unwrap();
    assert!(parsed.success(), "PostgreSQL's copy script parses");

    let root = std::env::temp_dir().join(format!("bbx-copy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = root.join("data");
    let stage = root.join("stage");
    let back = root.join("back");
    std::fs::create_dir_all(data.join("inner")).unwrap();
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::create_dir_all(&back).unwrap();
    std::fs::write(data.join("mail.db"), b"one message").unwrap();
    std::fs::write(data.join("inner/note"), b"kept").unwrap();
    let (data, stage, back) = (
        data.to_str().unwrap(),
        stage.to_str().unwrap(),
        back.to_str().unwrap(),
    );
    let missing = format!("{data}-not-there");
    assert_eq!(
        sh(PACK, &[stage, data, &missing]).0,
        0,
        "a path not there is left out"
    );
    let (_, size) = sh(SIZE, &[data, &missing]);
    assert!(size.trim().parse::<u64>().unwrap() > 0);
    let copy = format!("{stage}/copy");
    let (_, summed) = sh(SUM, &[&copy]);
    assert!(summed.starts_with("bytes=") && summed.contains("\nsha="));

    // Out a piece at a time, in again the same way.
    let (_, piece) = sh(PIECE, &[&copy, "0"]);
    std::fs::write(format!("{back}/piece"), piece).unwrap();
    assert_eq!(sh(APPEND, &[back]).0, 0);
    assert_eq!(sh(SUM, &[&format!("{back}/copy")]).1, summed);

    std::fs::write(format!("{data}/mail.db"), b"two messages").unwrap();
    std::fs::write(format!("{data}/extra"), b"new").unwrap();
    assert_eq!(sh(UNPACK, &[back, data]).0, 0);
    assert_eq!(
        std::fs::read(format!("{data}/mail.db")).unwrap(),
        b"one message"
    );
    assert_eq!(
        std::fs::read(format!("{data}/inner/note")).unwrap(),
        b"kept"
    );
    assert!(!std::path::Path::new(&format!("{data}/extra")).exists());
    assert_eq!(
        std::fs::read(format!("{back}/old{data}/mail.db")).unwrap(),
        b"two messages",
        "what was there waits in the stage"
    );

    // The stage is the only thing these ever create or delete.
    assert_eq!(sh(STAGE, &[stage]).0, 2);
    sh(UNSTAGE, &[stage]);
    assert!(std::path::Path::new(stage).exists());
    let _ = std::fs::remove_dir_all(&root);
}
