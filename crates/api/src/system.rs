//! What this machine can actually run.
//!
//! `system.probe`; the language half is the shell's. Every answer carries a
//! reason, because "QEMU: false" gives nobody anything to act on. Frida is
//! whether `frida` is on `PATH`, at the version it reports.

use std::path::Path;
use std::process::{Command, Stdio};

use beatbox_contract::{Availability, SystemProbe};
use beatbox_engine::{Reason, reason};
use beatbox_isolation::host_os;
use beatbox_isolation::machine::{KVM_DEVICE, Qemu, kvm_available};
use serde_json::Value;

use crate::Api;
use crate::fault::ApiFault;
use crate::sandbox::reason_view;

fn says(ready: bool, reason: Reason) -> Availability {
    Availability {
        ready,
        reason: reason_view(&reason),
    }
}

/// What decides the `kvm` answer, handed in so tests can fake it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KvmFacts {
    /// `/dev/kvm` opens for this user.
    pub usable: bool,
    /// This is Windows.
    pub windows: bool,
    /// Windows' hypervisor platform answers.
    pub whpx: bool,
    /// This is Linux.
    pub linux: bool,
    /// `/dev/kvm` is there at all.
    pub device: bool,
}

impl KvmFacts {
    /// This host's.
    pub fn here() -> Self {
        Self {
            usable: kvm_available(),
            windows: host_os::WINDOWS,
            whpx: host_os::WINDOWS && host_os::whpx_available(),
            linux: host_os::LINUX,
            device: Path::new(KVM_DEVICE).exists(),
        }
    }
}

/// Whether a guest here runs on a hypervisor, and why not. Still called
/// `kvm` on Windows, where the answer is WHPX.
pub fn kvm(facts: KvmFacts) -> Availability {
    if facts.usable {
        return says(true, reason!("probe.kvm.ready"));
    }
    if facts.windows {
        return if facts.whpx {
            says(true, reason!("probe.kvm.whpx_ready"))
        } else {
            says(false, reason!("probe.kvm.whpx_missing"))
        };
    }
    if !facts.linux {
        return says(false, reason!("probe.kvm.not_linux"));
    }
    // There but not ours: Debian, Ubuntu and Fedora give /dev/kvm to the kvm
    // group only, and a user outside it gets every machine emulated, ten
    // times slower, with no other sign.
    if facts.device {
        says(false, reason!("probe.kvm.no_permission"))
    } else {
        says(false, reason!("probe.kvm.missing"))
    }
}

fn qemu() -> Availability {
    if Qemu::here().system_found {
        says(true, reason!("probe.qemu.ready"))
    } else {
        says(false, reason!("probe.qemu.missing"))
    }
}

fn frida() -> Availability {
    let version = Command::new("frida")
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    match version {
        Some(version) => says(true, reason!("probe.frida.ready", version = version)),
        None => says(false, reason!("probe.frida.missing")),
    }
}

/// The terminal, POSIX only. On Windows say there's none rather than draw
/// one that swallows keystrokes.
fn terminal() -> Availability {
    if cfg!(unix) {
        says(true, reason!("probe.terminal.ready"))
    } else {
        says(false, reason!("probe.terminal.no_pty"))
    }
}

/// Who to greet: the account name, nothing more personal.
pub fn user() -> String {
    for name in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
        if let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) {
            return value.to_string_lossy().into_owned();
        }
    }
    account_name().unwrap_or_default()
}

#[cfg(unix)]
fn account_name() -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").ok()?.uid().to_string();
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() > 2 && fields[2] == uid).then(|| fields[0].to_owned())
    })
}

#[cfg(not(unix))]
fn account_name() -> Option<String> {
    None
}

impl Api {
    pub(crate) fn system_probe(&self) -> Result<Value, ApiFault> {
        let (free, total) = host_os::memory_mb();
        let mebibytes = |value: Option<u64>| i64::try_from(value.unwrap_or(0)).unwrap_or(i64::MAX);
        let probe = SystemProbe {
            platform: format!("{} {}", host_os::system_name(), host_os::release())
                .trim()
                .to_owned(),
            user: user(),
            kvm: kvm(KvmFacts::here()),
            qemu: qemu(),
            frida: frida(),
            terminal: terminal(),
            memory_free_mb: mebibytes(free),
            memory_total_mb: mebibytes(total),
        };
        Ok(serde_json::to_value(probe).expect("a probe is JSON"))
    }
}
