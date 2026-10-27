//! What every host suite wants: a room, a runtime with the host type in it,
//! a guard that takes every guest down, and one QEMU at a time.

#![allow(dead_code)]

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use beatbox_engine::{Recorder, RuntimeParts, SandboxRegistry, SandboxRuntime};
use beatbox_host::{Environment, HostSandbox};
use beatbox_isolation::machine::{ISO_MAGIC, ISO_MAGIC_OFFSET, Qemu};

/// A directory for one test to scribble in, removed when it is dropped.
pub struct Room(PathBuf);

impl Room {
    pub fn new(prefix: &str) -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "beatbox-host-tests-{}-{prefix}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(beatbox_isolation::host_os::canonical(&path).unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    pub fn text(&self, relative: &str) -> String {
        self.join(relative).to_string_lossy().into_owned()
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A file that sniffs as a disc. `is_optical` reads one magic string.
pub fn fake_disc(path: &Path) -> String {
    let mut data = vec![0u8; ISO_MAGIC_OFFSET as usize + ISO_MAGIC.len() + 4];
    data[ISO_MAGIC_OFFSET as usize..ISO_MAGIC_OFFSET as usize + ISO_MAGIC.len()]
        .copy_from_slice(ISO_MAGIC);
    std::fs::write(path, data).unwrap();
    path.to_string_lossy().into_owned()
}

/// A runtime whose sandboxes are destroyed when it goes, so no test leaves a
/// guest running behind it.
pub struct Guarded(pub SandboxRuntime);

impl Deref for Guarded {
    type Target = SandboxRuntime;
    fn deref(&self) -> &SandboxRuntime {
        &self.0
    }
}

impl DerefMut for Guarded {
    fn deref_mut(&mut self) -> &mut SandboxRuntime {
        &mut self.0
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        self.0.destroy_all();
    }
}

/// A runtime with the host type in it, asking `environment`, and everything
/// recorded.
pub fn build_with(environment: Environment) -> (Guarded, Recorder) {
    let mut registry = SandboxRegistry::default();
    registry
        .register(HostSandbox::spec_with(environment))
        .unwrap();
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let log = Recorder::default();
    runtime.events().subscribe("*", log.sink()).unwrap();
    (Guarded(runtime), log)
}

/// The same, on this computer as it is -- except that a guest asked to shut
/// down is given half a second, not a minute: the disks these tests boot hold
/// no system to hear the power button, and every stop would wait it out.
pub fn build() -> (Guarded, Recorder) {
    build_with(Environment {
        shutdown_grace: GRACE,
        ..Environment::here()
    })
}

/// See [`build`].
pub const GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// This computer, except that it can always show a window.
pub fn with_a_screen() -> Environment {
    let mut environment = Environment::here();
    environment.display_available = Arc::new(|_| true);
    environment
}

/// A QEMU found by its bare name, for building an argv to read.
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

/// Whether a real guest can be started here. Without one a test says so on
/// stderr and passes: the harness has no skip, and absent isn't broken.
pub fn have_qemu() -> bool {
    let here = Qemu::here();
    here.system_found && here.image_found
}

/// One real guest at a time: each start checks free memory, and fifteen at
/// once would refuse each other.
pub fn one_guest_at_a_time() -> MutexGuard<'static, ()> {
    static QEMU: Mutex<()> = Mutex::new(());
    QEMU.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An empty qcow2 of 64 MiB: no operating system at all, which is the point.
pub fn empty_disk(path: &Path) -> String {
    let status = Command::new("qemu-img")
        .args(["create", "-f", "qcow2"])
        .arg(path)
        .arg("64M")
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    path.to_string_lossy().into_owned()
}

/// Skip the rest of this test where there is no QEMU, saying so.
#[macro_export]
macro_rules! needs_qemu {
    () => {
        if !support::have_qemu() {
            eprintln!("skipped: QEMU is not installed on this host");
            return;
        }
    };
}
