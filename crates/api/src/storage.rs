//! What BeatBox keeps on this computer's disk, and clearing what nobody
//! needs: `storage.usage` and `storage.sweep`, for Settings' Storage.
//!
//! Sizes are what the files take on the disk, not how long they say they
//! are: a machine's overlay and a raw disk are sparse, and their length is
//! what the guest could write, not what it has.
//!
//! A disk names the machines made from it by id (`made_into`), so the page
//! can delete them with it.
//!
//! The sweep is the one a start does (`leftovers.rs`), offered on demand: a
//! room goes only when no sandbox BeatBox knows claims it and no QEMU holds
//! it. Only the daemon that owns the machines sweeps, as at start.

use std::fs;
use std::path::Path;

use beatbox_contract::{StorageView, StoredDisk, StoredMachine};
use serde_json::Value;

use crate::fault::ApiFault;
use crate::{Api, leftovers, py};

/// What a file takes on the disk.
fn taken(meta: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Everything under `path`, symlinks not followed. Nothing, when it is not
/// there.
pub(crate) fn size_of(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return taken(&meta);
    }
    fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| size_of(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

fn int(bytes: u64) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

impl Api {
    fn setting(sandbox: &beatbox_engine::Sandbox, key: &str) -> String {
        sandbox
            .config()
            .setting(key)
            .map(py::text_or_empty)
            .unwrap_or_default()
    }

    fn storage(&self) -> StorageView {
        let sandboxes = self.runtime.sandboxes();
        let machines = sandboxes
            .iter()
            .filter_map(|sandbox| {
                let room = Self::setting(sandbox, "workspace");
                (!room.is_empty()).then(|| StoredMachine {
                    id: sandbox.id(),
                    name: sandbox.name(),
                    r#type: sandbox.type_name(),
                    lifecycle: sandbox.state().lifecycle.as_str().to_owned(),
                    bytes: int(size_of(Path::new(&room))),
                })
            })
            .collect();
        let disks = self
            .disks()
            .into_iter()
            .map(|disk| {
                let path = leftovers::resolved(Path::new(&disk.path));
                let made_into = sandboxes
                    .iter()
                    .filter(|sandbox| {
                        let made_from = Self::setting(sandbox, "disk");
                        !made_from.is_empty() && leftovers::resolved(Path::new(&made_from)) == path
                    })
                    .map(|sandbox| sandbox.id())
                    .collect();
                StoredDisk {
                    name: disk.name,
                    bytes: disk.bytes,
                    made_into,
                }
            })
            .collect();
        let leftover: u64 = leftovers::abandoned(
            &self.paths.machines,
            &self.paths.temp,
            &leftovers::claimed(&self.runtime),
        )
        .iter()
        .map(|room| size_of(room))
        .sum();
        StorageView {
            folder: self.paths.root.to_string_lossy().into_owned(),
            machines,
            disks,
            tools_bytes: int(size_of(&self.paths.tools)),
            leftover_bytes: int(leftover),
        }
    }

    pub(crate) fn storage_usage(&self) -> Result<Value, ApiFault> {
        Ok(serde_json::to_value(self.storage()).expect("JSON"))
    }

    pub(crate) fn storage_sweep(&self) -> Result<Value, ApiFault> {
        if !self.owns() {
            return Err(ApiFault::new(
                "not-owner",
                "Another BeatBox owns these machines; only it can clear what they left.",
            ));
        }
        self.sweep_leftovers(&leftovers::claimed(&self.runtime));
        self.storage_usage()
    }
}
