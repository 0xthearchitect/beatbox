//! Scratch space that goes away when the test does, and a QEMU that is not
//! looked for.
//!
//! A [`Room`] removes itself when dropped, so `/tmp` doesn't fill with
//! thousands of stray `beatbox-*` folders.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use beatbox_isolation::machine::Qemu;

/// A directory for one test to scribble in, removed when it is dropped.
pub struct Room(PathBuf);

impl Room {
    pub fn new(prefix: &str) -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("beatbox-tests-{}-{prefix}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(beatbox_isolation::host_os::canonical(&path).unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// `relative`, made, holding `bytes`.
    pub fn file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A QEMU found by its bare name, on a host with no hypervisor.
pub fn qemu() -> Qemu {
    Qemu {
        system: "qemu-system-x86_64".into(),
        image: "qemu-img".into(),
        system_found: true,
        image_found: true,
        kvm: false,
        whpx: false,
        cpu: None,
    }
}

/// Enough of an ISO 9660 image for `is_optical` to call it one.
pub fn a_disc() -> Vec<u8> {
    let offset = beatbox_isolation::machine::ISO_MAGIC_OFFSET as usize;
    let mut image = vec![0u8; offset + 64];
    image[offset..offset + 5].copy_from_slice(beatbox_isolation::machine::ISO_MAGIC);
    image
}
