//! Where BeatBox keeps things on this computer.
//!
//! The same places the Python build used, so an upgrade finds its machines:
//!
//! | | |
//! |---|---|
//! | `state` | `~/.beatbox/state/<id>.json`, one record per sandbox, and `.owner` |
//! | `machines` | `~/.beatbox/machines/sbx-*`, one room per machine: its disk and sockets |
//! | `temp` | the temporary directory: where rooms were kept before 2026-09-23, and are still swept from, and where an install's two sockets live |
//! | `disks` | `~/.beatbox/disks`, the disks BeatBox made |
//! | `tools` | `~/.beatbox/tools`, the one download |
//!
//! `BEATBOX_HOME` moves the first two, for scripts that open the real
//! window. `disks` and `tools` stay under the home directory.
//! ponytail: BEATBOX_HOME should move those too; kept as the Python had it.

use std::path::{Path, PathBuf};

/// The folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `~/.beatbox`, or `BEATBOX_HOME`: where the daemon's log is kept.
    pub root: PathBuf,
    /// Records, and the owner's lock.
    pub state: PathBuf,
    /// Machines' rooms.
    pub machines: PathBuf,
    /// The temporary directory. See the table above.
    pub temp: PathBuf,
    /// Disks BeatBox made.
    pub disks: PathBuf,
    /// The guest tools.
    pub tools: PathBuf,
    /// Service definitions: drop a declarative plugin's file here to install it.
    pub services: PathBuf,
}

fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

impl Paths {
    /// This user's.
    pub fn here() -> Self {
        let beatbox = std::env::var_os("BEATBOX_HOME")
            .filter(|value| !value.is_empty())
            .map_or_else(|| home().join(".beatbox"), PathBuf::from);
        Self {
            root: beatbox.clone(),
            state: beatbox.join("state"),
            machines: beatbox.join("machines"),
            temp: std::env::temp_dir(),
            disks: home().join(".beatbox").join("disks"),
            tools: home().join(".beatbox").join("tools"),
            services: beatbox.join("services"),
        }
    }

    /// Everything under `root`: for tests, and for a daemon that must not
    /// touch anybody's machines.
    pub fn under(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            state: root.join("state"),
            machines: root.join("machines"),
            temp: root.join("tmp"),
            disks: root.join("disks"),
            tools: root.join("tools"),
            services: root.join("services"),
        }
    }
}
