//! Rooms machines from an earlier run left behind, and the sweep that
//! removes them.
//!
//! A machine whose BeatBox was killed never removes its room, and that's
//! gigabytes of somebody's disk. A room is kept if a restored sandbox owns
//! it (running or not), or else if a QEMU still answers on its monitor (a
//! room with no readable record).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use beatbox_engine::SandboxRuntime;
use beatbox_isolation::{endpoint, host_os};

use crate::py;

/// The real path, or the absolute one if it isn't there yet.
pub(crate) fn resolved(path: &Path) -> PathBuf {
    host_os::canonical(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Every workspace a live sandbox still points to, resolved.
///
/// From each sandbox's `workspace` setting, not assumed under `machines/`,
/// so a room kept elsewhere is protected too.
pub(crate) fn claimed(runtime: &SandboxRuntime) -> BTreeSet<PathBuf> {
    runtime
        .sandboxes()
        .iter()
        .filter_map(|sandbox| {
            let room = sandbox
                .config()
                .setting("workspace")
                .map(py::text_or_empty)
                .unwrap_or_default();
            (!room.is_empty()).then(|| resolved(Path::new(&room)))
        })
        .collect()
}

/// Every room nobody claims and no QEMU holds: what a sweep removes.
pub(crate) fn abandoned(machines: &Path, temp: &Path, claimed: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut rooms = rooms_in(machines, "sbx-");
    rooms.extend(rooms_in(temp, "beatbox-host-sbx-"));
    rooms
        .into_iter()
        .filter(|room| {
            room.is_dir()
                && !claimed.contains(&resolved(room))
                && !in_use(&room.join("monitor.sock"))
        })
        .collect()
}

/// Remove every room nobody claims and no QEMU holds, and name them.
pub(crate) fn sweep(machines: &Path, temp: &Path, claimed: &BTreeSet<PathBuf>) -> Vec<String> {
    let mut removed = Vec::new();
    for room in abandoned(machines, temp, claimed) {
        let _ = fs::remove_dir_all(&room);
        removed.push(room.to_string_lossy().into_owned());
    }
    if !removed.is_empty() {
        log::info!(
            "BeatBox: removed {} machine(s) left from an earlier run",
            removed.len()
        );
    }
    removed
}

fn rooms_in(folder: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut rooms: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .map(|entry| entry.path())
        .collect();
    rooms.sort();
    rooms
}

/// Whether a QEMU still answers on this channel: a UNIX socket on Linux, the
/// port recorded in this file on Windows (`endpoint`).
pub(crate) fn in_use(path: &Path) -> bool {
    path.exists() && endpoint::connect(path, Duration::from_millis(500)).is_ok()
}

/// A room for one new machine, under `machines`: `sbx-` and twelve hex
/// digits.
pub(crate) fn workspace(machines: &Path) -> String {
    let id: String = beatbox_isolation::uuid4()
        .replace('-', "")
        .chars()
        .take(12)
        .collect();
    machines
        .join(format!("sbx-{id}"))
        .to_string_lossy()
        .into_owned()
}
