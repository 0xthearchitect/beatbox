//! What BeatBox needs from this computer, and the one step for each that
//! BeatBox can take itself.
//!
//! QEMU with a window (nothing runs without it), a hypervisor this user can
//! reach (or every machine is ten times slower), and, optionally, the
//! Windows guest tools. Read the way `system.probe` reads them.
//!
//! **Each fix is one fixed command, elevated by the system**: `pkexec` on
//! Linux, UAC on Windows. BeatBox never sees a password, and nothing a
//! caller sends reaches a command: the step is one of three names, the rest
//! literal, except the account joining the kvm group, which is our own and
//! checked first.
//!
//! What can't be fixed from here is said, not offered: virtualization off in
//! the firmware, an unknown distribution, no `pkexec`.

use std::sync::Arc;
use std::time::Duration;

use beatbox_contract::{ReasonView, SetupStepView, SetupView};
use beatbox_engine::{Reason, reason};
use beatbox_isolation::host_os;
use beatbox_isolation::machine::{self, Qemu};
use beatbox_isolation::runner::{Output, Runner, SystemRunner};
use serde_json::{Map, Value};

use crate::Api;
use crate::fault::ApiFault;
use crate::guest_tools::run;
use crate::jobs::Said;
use crate::sandbox::reason_view;
use crate::system::{KvmFacts, kvm, user};

/// The kind of job a fix is.
pub const JOB: &str = "setup";
/// QEMU, `qemu-img` and a windowed display.
pub const QEMU: &str = "qemu";
/// KVM for this user, or Windows' Hypervisor Platform.
pub const ACCELERATION: &str = "acceleration";
/// The virtio-win guest tools, for Windows guests.
pub const GUEST_TOOLS: &str = "guest_tools";
/// Every step, in the order they are done.
pub const STEPS: [&str; 3] = [QEMU, ACCELERATION, GUEST_TOOLS];

/// A fix that counts only after logging out and back in.
pub const RELOGIN: &str = "relogin";
/// A fix that counts only after restarting the computer.
pub const RESTART: &str = "restart";

/// How long a fix may take: a package manager on a slow mirror, behind a
/// password dialog nobody has answered yet.
pub const FIX_WITHIN: Duration = Duration::from_secs(30 * 60);

/// QEMU in winget, which is the build from qemu.weilnetz.de that BeatBox
/// was tested with on Windows, and installs where BeatBox already looks.
pub const WINGET_QEMU: &str = "SoftwareFreedomConservancy.QEMU";

/// What gives each family QEMU, `qemu-img`, a window and UEFI (OVMF), as one
/// line run as root. Display packages match `machine::DISPLAY_PACKAGES`.
/// apt's lists are refreshed first: a desktop's can be old enough to 404.
pub const QEMU_PACKAGES: [(&[&str], &str); 4] = [
    (
        &["debian", "ubuntu"],
        "apt-get update && apt-get install -y qemu-system-x86 qemu-utils qemu-system-gui ovmf",
    ),
    (
        &["fedora", "rhel", "centos"],
        "dnf install -y qemu-system-x86 qemu-img qemu-ui-gtk edk2-ovmf",
    ),
    (
        &["suse", "opensuse"],
        "zypper --non-interactive install qemu-x86 qemu-tools qemu-ui-gtk qemu-ovmf-x86_64",
    ),
    (
        &["arch"],
        "pacman -S --needed --noconfirm qemu-system-x86 qemu-img qemu-ui-gtk edk2-ovmf",
    ),
];

/// Turns the Hypervisor Platform on, elevated, and exits with DISM's code.
const WHPX_ON: &str = "$p = Start-Process -FilePath dism.exe -ArgumentList \
    '/online','/enable-feature','/featurename:HypervisorPlatform','/all','/norestart' \
    -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode";
/// DISM's "done, restart to finish".
const DISM_RESTART: i32 = 3010;
/// `pkexec`'s "the dialog was dismissed".
const PKEXEC_DISMISSED: i32 = 126;
/// Windows' ERROR_CANCELLED as an HRESULT: UAC said No, or nobody answered
/// it. winget prints it as text as well as, or instead of, exiting with it.
const UAC_DECLINED: u32 = 0x8007_04C7;

/// What decides every step, read from this computer by [`Host::here`] and
/// handed in by the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// This is Windows.
    pub windows: bool,
    /// `qemu-system-x86_64` and `qemu-img` are both here.
    pub qemu: bool,
    /// QEMU can open a window.
    pub window: bool,
    /// What decides the hypervisor.
    pub kvm: KvmFacts,
    /// This user is listed in the kvm group, whether or not this session
    /// has it yet.
    pub in_kvm_group: bool,
    /// `/etc/os-release`.
    pub os_release: String,
    /// `pkexec` is on `PATH`.
    pub pkexec: bool,
    /// `winget` is on `PATH`.
    pub winget: bool,
    /// Who is logged in.
    pub user: String,
}

impl Host {
    /// This computer's, now.
    pub fn here() -> Self {
        let qemu = Qemu::here();
        let windows = host_os::WINDOWS;
        let user = user();
        Self {
            windows,
            qemu: qemu.system_found && qemu.image_found,
            // Every QEMU for Windows has GTK and SDL built in, and asking it
            // takes seconds there.
            window: qemu.system_found
                && (windows || machine::display_available(&qemu, &SystemRunner)),
            kvm: KvmFacts::here(),
            in_kvm_group: std::fs::read_to_string("/etc/group")
                .is_ok_and(|groups| in_group(&groups, "kvm", &user)),
            os_release: std::fs::read_to_string("/etc/os-release").unwrap_or_default(),
            pkexec: !windows && host_os::on_path("pkexec"),
            winget: windows && host_os::on_path("winget"),
            user,
        }
    }
}

/// Whether `/etc/group` lists `user` in `group`.
pub fn in_group(groups: &str, group: &str, user: &str) -> bool {
    groups.lines().any(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        fields.len() == 4 && fields[0] == group && fields[3].split(',').any(|m| m == user)
    })
}

/// An account name `usermod` takes, and nothing that could be read as an
/// option.
fn plain_account(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

/// This family's line from [`QEMU_PACKAGES`].
fn qemu_line(os_release: &str) -> Option<&'static str> {
    let family = machine::os_family(os_release);
    QEMU_PACKAGES
        .iter()
        .find(|(names, _)| family.iter().any(|member| names.contains(member)))
        .map(|(_, line)| *line)
}

/// One step: where it stands, and the command that would fix it, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// What the page is told.
    pub view: SetupStepView,
    /// What runs when it is fixed from here.
    pub fix: Option<Vec<String>>,
}

fn step(key: &str, ready: bool, reason: &Reason, fix: Option<Vec<String>>) -> Step {
    said(key, ready, reason_view(reason), fix)
}

fn said(key: &str, ready: bool, reason: ReasonView, fix: Option<Vec<String>>) -> Step {
    Step {
        view: SetupStepView {
            key: key.into(),
            ready,
            optional: key == GUEST_TOOLS,
            fixable: !ready && fix.is_some(),
            reason,
        },
        fix: if ready { None } else { fix },
    }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|&word| word.to_owned()).collect()
}

/// QEMU with a window.
pub fn qemu_step(host: &Host) -> Step {
    if host.qemu && host.window {
        return step(QEMU, true, &reason!("setup.qemu.ready"), None);
    }
    if host.windows {
        return if host.winget {
            let fix = argv(&[
                "winget",
                "install",
                "--id",
                WINGET_QEMU,
                "--exact",
                "--silent",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--disable-interactivity",
            ]);
            step(QEMU, false, &reason!("setup.qemu.winget"), Some(fix))
        } else {
            let why = reason!("setup.qemu.download", url = machine::display_install_hint());
            step(QEMU, false, &why, None)
        };
    }
    let Some(line) = qemu_line(&host.os_release) else {
        return step(QEMU, false, &reason!("setup.qemu.unknown_system"), None);
    };
    if !host.pkexec {
        let why = reason!("setup.qemu.by_hand", command = line);
        return step(QEMU, false, &why, None);
    }
    let why = if host.qemu {
        reason!("setup.qemu.no_window", command = line)
    } else {
        reason!("setup.qemu.missing", command = line)
    };
    step(QEMU, false, &why, Some(argv(&["pkexec", "sh", "-c", line])))
}

/// A hypervisor this user can reach.
pub fn acceleration_step(host: &Host) -> Step {
    let found = kvm(host.kvm);
    if found.ready {
        return said(ACCELERATION, true, found.reason, None);
    }
    if host.kvm.windows {
        let fix = argv(&[
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WHPX_ON,
        ]);
        return step(
            ACCELERATION,
            false,
            &reason!("setup.acceleration.whpx"),
            Some(fix),
        );
    }
    if host.kvm.linux && host.kvm.device {
        if host.in_kvm_group {
            return step(
                ACCELERATION,
                false,
                &reason!("setup.acceleration.relogin"),
                None,
            );
        }
        if host.pkexec && plain_account(&host.user) {
            let fix = argv(&["pkexec", "usermod", "-aG", "kvm", &host.user]);
            let why = reason!("setup.acceleration.kvm_group", user = host.user);
            return step(ACCELERATION, false, &why, Some(fix));
        }
    }
    // Off in the firmware, or not a system with either: nothing from here.
    said(ACCELERATION, false, found.reason, None)
}

/// The Windows guest tools. Fetched by `tools.fetch`, not by a command.
pub fn guest_tools_step(ready: bool, version: &str, bytes: u64) -> Step {
    if ready {
        return step(
            GUEST_TOOLS,
            true,
            &reason!("setup.guest_tools.ready", version = version),
            None,
        );
    }
    let why = reason!(
        "setup.guest_tools.missing",
        megabytes = bytes.div_ceil(1 << 20)
    );
    step(GUEST_TOOLS, false, &why, Some(Vec::new()))
}

/// What a fix that ran is read as.
pub fn outcome(key: &str, windows: bool, ran: std::io::Result<Output>) -> (bool, Said) {
    let after = |key: &str| match key {
        ACCELERATION if windows => RESTART,
        ACCELERATION => RELOGIN,
        _ => "",
    };
    match ran {
        Ok(output)
            if output.code == 0
                || (windows && key == ACCELERATION && output.code == DISM_RESTART) =>
        {
            (true, Said::Text(after(key).into()))
        }
        Ok(output) if !windows && output.code == PKEXEC_DISMISSED => {
            (false, Said::Reason(reason!("setup.dismissed")))
        }
        Ok(output)
            if windows
                && (output.code as u32 == UAC_DECLINED
                    || format!("{}{}", output.stdout, output.stderr)
                        .to_ascii_lowercase()
                        .contains("0x800704c7")) =>
        {
            (false, Said::Reason(reason!("setup.declined")))
        }
        Ok(output) => {
            let said = [&output.stderr, &output.stdout]
                .into_iter()
                .flat_map(|text| text.lines().rev())
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map_or_else(|| format!("exit {}", output.code), str::to_owned);
            // winget can die on its progress line, and only the code says why.
            let said = if windows {
                format!("{said} (0x{:08X})", output.code as u32)
            } else {
                said
            };
            (false, Said::Reason(reason!("setup.failed", detail = said)))
        }
        Err(error) => (false, Said::Reason(reason!("setup.failed", detail = error))),
    }
}

/// What a fix needs from this computer: its facts, and what runs the fix.
#[derive(Clone)]
pub struct Setup {
    /// This computer's facts, read again every time they are asked for.
    pub host: Arc<dyn Fn() -> Host + Send + Sync>,
    /// What runs the fix.
    pub runner: Arc<dyn Runner>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            host: Arc::new(Host::here),
            runner: Arc::new(SystemRunner),
        }
    }
}

impl Api {
    fn setup_steps(&self, host: &Host) -> [Step; 3] {
        let pinned = &self.tools.pinned;
        [
            qemu_step(host),
            acceleration_step(host),
            guest_tools_step(self.tools_ready(), &pinned.version, pinned.size),
        ]
    }

    pub(crate) fn setup_status(&self) -> Result<Value, ApiFault> {
        let host = (self.setup.host)();
        let view = SetupView {
            steps: self
                .setup_steps(&host)
                .into_iter()
                .map(|s| s.view)
                .collect(),
        };
        Ok(serde_json::to_value(view).expect("a view is JSON"))
    }

    /// Fix one step, as a job. Refused if it is fine already, or if it is
    /// nothing BeatBox can fix from here.
    pub(crate) fn setup_fix(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let key = crate::py::text_or_empty(&payload["step"]);
        let Some(&key) = STEPS.iter().find(|&&known| known == key) else {
            return Err(ApiFault::new(
                "bad-step",
                format!("There is no step called {key:?}."),
            ));
        };
        if key == GUEST_TOOLS {
            return self.tools_fetch();
        }
        let host = (self.setup.host)();
        let found = self
            .setup_steps(&host)
            .into_iter()
            .find(|s| s.view.key == key)
            .expect("every step is listed");
        if found.view.ready {
            return Err(ApiFault::new(
                "already",
                format!("{key} is already in place."),
            ));
        }
        let Some(fix) = found.fix else {
            return Err(ApiFault::new(
                "not-fixable",
                format!("BeatBox cannot put {key} in place from here."),
            ));
        };
        let runner = Arc::clone(&self.setup.runner);
        let windows = host.windows;
        let job = run(&self.jobs, JOB, move |say| {
            say(key);
            outcome(key, windows, runner.run(&fix, Some(FIX_WITHIN)))
        })?;
        Ok(crate::jobs::wire(&job))
    }
}
