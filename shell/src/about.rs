//! Settings' About: which BeatBox this is, the engine running the machines,
//! and opening its folder or the engine's log (`system.about`,
//! `host.reveal`).
//!
//! The log is the daemon's own, `beatboxd.log` in BeatBox's folder
//! (`crates/daemon/src/log.rs`); the window only reads its size.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

use beatbox_contract::AboutView;

use crate::link::Link;
use crate::{Fault, beatbox_home};

fn log_file() -> PathBuf {
    beatbox_home().join("beatboxd.log")
}

/// What About shows. The engine's build and process are empty when no
/// engine is running: asking does not start one.
pub fn about(link: &Link) -> AboutView {
    let status = link.status().unwrap_or_default();
    let log = log_file();
    AboutView {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        engine_build: status["build"].as_str().unwrap_or_default().to_owned(),
        engine_pid: status["pid"].as_i64().unwrap_or(0),
        folder: beatbox_home().to_string_lossy().into_owned(),
        log_bytes: std::fs::metadata(&log)
            .map(|meta| i64::try_from(meta.len()).unwrap_or(i64::MAX))
            .unwrap_or(0),
        log: log.to_string_lossy().into_owned(),
    }
}

/// This computer's own program for opening a file or a folder.
fn opener() -> &'static str {
    if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Open BeatBox's folder or the log. Only those two: the page names which,
/// never a path, so nothing else on the computer can be opened this way.
pub fn reveal(what: &str) -> Result<(), Fault> {
    let path = match what {
        "folder" => beatbox_home(),
        "log" => log_file(),
        _ => return Err(Fault::new("bad-choice", "what is one of: folder, log.")),
    };
    if !path.exists() {
        return Err(Fault::new(
            "not-found",
            format!("{} is not there yet.", path.display()),
        ));
    }
    let mut child = Command::new(opener())
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| Fault::new("reveal", format!("{}: {error}", opener())))?;
    // Reaped, so a finished opener does not linger as a zombie.
    thread::spawn(move || child.wait());
    Ok(())
}
