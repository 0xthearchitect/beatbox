//! One BeatBox window at a time.
//!
//! The daemon holds `state/.owner`; windows hold `state/.window` beside it.
//! When no daemon answers, the owner's lock is tried and let go at once (so
//! the daemon about to start can take it): held means an older in-process
//! BeatBox is already open.

use std::fs::{self, File, OpenOptions};
use std::path::Path;

fn lock(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        // LockFileEx wants read or write access; append is neither.
        .read(true)
        .open(path)
        .ok()?;
    file.try_lock().is_ok().then_some(file)
}

/// This window's claim, held for as long as the returned file is; `None`
/// when BeatBox is already open. `daemon_answers` is asked only when the
/// windows' own lock was free.
pub fn claim(state: &Path, daemon_answers: impl FnOnce() -> bool) -> Option<File> {
    fs::create_dir_all(state).ok()?;
    let window = lock(&state.join(".window"))?;
    if !daemon_answers() {
        // Taken and dropped at once: only asking whether somebody holds it.
        lock(&state.join(".owner"))?;
    }
    Some(window)
}
