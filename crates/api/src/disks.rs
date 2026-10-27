//! The disks a machine boots from, and the one way BeatBox makes one.
//!
//! BeatBox never downloads a disk: it makes an empty one and boots an
//! installer disc at it. The result is a `.qcow2` in `disks/`.
//!
//! **An install is a job, not a sandbox.** The guest powering off is
//! success here, which a sandbox runtime would read as the guest lost.
//!
//! **The one place BeatBox keeps what a guest writes**: no `-snapshot`, no
//! overlay. So the target must not exist yet, which stops an installer
//! being pointed at somebody's real disk.
//!
//! The guest shutting down ends an install. Nobody can detect "I'm done
//! preparing", so there's a button, and it presses the ACPI power button
//! instead of pulling the plug: this disk becomes every machine's master.
//! An agent channel tells the job when the installed system first answers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use beatbox_contract::DiskView;
use beatbox_engine::{EngineError, SandboxConfig, SandboxRuntime, Transition, reason};
use beatbox_isolation::firmware;
use beatbox_isolation::machine::{
    self, DiskBuild, MachineError, MachineSpec, QEMU_BINARY, Qemu, RunningMachine,
};
use beatbox_isolation::qmp::{GuestAgent, Monitor};
use beatbox_isolation::runner::Runner;
use beatbox_isolation::{endpoint, host_os};
use serde_json::{Map, Value};

use crate::fault::ApiFault;
use crate::guest_tools::run;
use crate::jobs::{Said, Say};
use crate::{Api, py};

/// What a disk's file ends in.
pub const SUFFIX: &str = ".qcow2";
/// Every file the disks folder holds a disk in: BeatBox's own, a disk
/// taken out of an `.ova`, and a physical disk captured by BeatDrop.
pub const SUFFIXES: [&str; 3] = [
    SUFFIX,
    crate::foreign::UNPACKED_SUFFIX,
    crate::beatdrop::SUFFIX,
];
/// The job's own name, for the page to recognise its events by.
pub const KIND: &str = "disk";
/// The guest is up and somebody is working through the installer.
pub const WORKING: &str = "working";
/// Something inside answered: what was installed has booted and is up.
pub const READY: &str = "ready";
/// They said they are done, and it is being shut down.
pub const FINISHING: &str = "finishing";
/// How often to ask whether anything inside answers. Rarely: a fresh
/// connection each time, for something tens of minutes away, if ever.
pub const LISTEN_EVERY: Duration = Duration::from_secs(5);
/// How long a guest gets to shut down before the plug is pulled. Windows
/// applying updates on the way out uses most of it, and a hard stop on the
/// master image is worse than waiting.
pub const GRACE: Duration = Duration::from_secs(120);

const TICK: Duration = Duration::from_millis(500);

/// An installer's QEMU, as the install sees it.
pub trait Guest: Send {
    /// Whether it is still there.
    fn running(&mut self) -> bool;
    /// Ask it to go, then insist, waiting up to `timeout` for each.
    fn stop(&mut self, timeout: Duration);
    /// Whether, once gone, it switched itself off rather than being killed
    /// (usually by the OOM killer, mid-install).
    fn ended_well(&mut self) -> bool {
        true
    }
}

impl Guest for RunningMachine {
    fn running(&mut self) -> bool {
        RunningMachine::running(self)
    }

    fn stop(&mut self, timeout: Duration) {
        RunningMachine::stop(self, timeout);
    }

    fn ended_well(&mut self) -> bool {
        self.exit_status().is_none_or(|status| status.success())
    }
}

/// Starting an installer's QEMU from its argv, so tests can stand in.
pub trait Launch: Send + Sync {
    /// Spawn `argv`. The disk it installs to is already made.
    fn launch(&self, argv: &[String]) -> std::io::Result<Box<dyn Guest>>;
}

/// Spawns QEMU, for real: its output to nowhere, since a pipe nobody reads
/// is a guest that freezes when it fills.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spawn;

impl Launch for Spawn {
    fn launch(&self, argv: &[String]) -> std::io::Result<Box<dyn Guest>> {
        let (program, arguments) = argv
            .split_first()
            .ok_or_else(|| std::io::Error::other("nothing to run"))?;
        let child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Box::new(RunningMachine::new(
            MachineSpec::default(),
            child,
            false,
        )))
    }
}

/// What an install needs from this computer.
#[derive(Clone)]
pub struct Installer {
    /// Where QEMU is.
    pub qemu: Arc<dyn Fn() -> Qemu + Send + Sync>,
    /// Whether it can show a window, and what to install if not.
    pub display: Arc<dyn Fn(&Qemu) -> machine::Display + Send + Sync>,
    /// What runs `qemu-img`.
    pub runner: Arc<dyn Runner>,
    /// What starts the installer.
    pub launch: Arc<dyn Launch>,
    /// A fresh SMBIOS UUID for every install.
    pub uuid: Arc<dyn Fn() -> String + Send + Sync>,
    /// See [`GRACE`].
    pub grace: Duration,
    /// See [`LISTEN_EVERY`].
    pub listen_every: Duration,
}

impl Default for Installer {
    fn default() -> Self {
        Self {
            qemu: Arc::new(Qemu::here),
            display: Arc::new(|qemu| {
                machine::Display::here(qemu, &beatbox_isolation::runner::SystemRunner)
            }),
            runner: Arc::new(beatbox_isolation::runner::SystemRunner),
            launch: Arc::new(Spawn),
            uuid: Arc::new(beatbox_isolation::uuid4),
            grace: GRACE,
            listen_every: LISTEN_EVERY,
        }
    }
}

/// Set once, waited on with a timeout.
#[derive(Default)]
struct Flag {
    set: Mutex<bool>,
    changed: Condvar,
}

impl Flag {
    fn set(&self) {
        *self.set.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let set = self.set.lock().unwrap_or_else(PoisonError::into_inner);
        let (set, _) = self
            .changed
            .wait_timeout_while(set, timeout, |set| !*set)
            .unwrap_or_else(PoisonError::into_inner);
        *set
    }
}

type Shared = Arc<Mutex<Box<dyn Guest>>>;

/// The install that is up, if one is. One at a time; the jobs say so too.
#[derive(Default)]
pub(crate) struct Install {
    running: Mutex<Option<Shared>>,
    done: Mutex<Arc<Flag>>,
}

impl Install {
    fn guest(&self) -> Option<Shared> {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Whether an install is up. What the window asks before it closes.
    pub(crate) fn building(&self) -> bool {
        self.guest().is_some_and(|guest| {
            guest
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .running()
        })
    }
}

fn alive(guest: &Shared) -> bool {
    guest
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .running()
}

/// What an ended install left on its disk. Powering off also ends a live
/// session nobody installed from, so the disk is read: empty means no
/// install and it goes; something unbootable is an unfinished install and
/// stays for whoever wants to look. An unreadable disk is taken at its word.
pub fn installed(target: &str, qemu: &Qemu, runner: &dyn Runner, scratch: &Path) -> (bool, Said) {
    let Some(head) = firmware::disk_head(target, "qcow2", qemu, runner, scratch) else {
        return (true, Said::Text(target.to_owned()));
    };
    if firmware::blank(&head) {
        let _ = fs::remove_file(target);
        return (false, Said::Reason(reason!("disks.nothing_installed")));
    }
    if !firmware::bootable(&head) {
        return (false, Said::Reason(reason!("disks.install_unfinished")));
    }
    (true, Said::Text(target.to_owned()))
}

/// A name, not a path: `^[A-Za-z0-9][A-Za-z0-9 ._-]{0,63}$`. A separator
/// would let a caller write outside BeatBox's folder.
pub(crate) fn a_name(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && text.chars().count() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

pub(crate) fn bad_name(name: &Value) -> ApiFault {
    ApiFault::new(
        "bad-name",
        format!("A disk name is a name, not a path: {}", py::repr(name)),
    )
}

impl Api {
    /// The disks BeatBox has made, newest first. Only its own folder, never
    /// a scan of somebody's home.
    pub fn disks(&self) -> Vec<DiskView> {
        let Ok(entries) = fs::read_dir(&self.paths.disks) else {
            return Vec::new();
        };
        let mut found: Vec<(SystemTime, PathBuf, u64)> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| SUFFIXES.iter().any(|suffix| name.ends_with(suffix)))
            })
            .filter_map(|path| {
                let meta = fs::metadata(&path).ok()?;
                let bytes = if path.extension().is_some_and(|extension| extension == "raw") {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        meta.blocks().saturating_mul(512)
                    }
                    #[cfg(not(unix))]
                    {
                        meta.len()
                    }
                } else {
                    meta.len()
                };
                Some((meta.modified().ok()?, path, bytes))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        found
            .into_iter()
            .map(|(_, path, bytes)| DiskView {
                name: path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: path.to_string_lossy().into_owned(),
                bytes: i64::try_from(bytes).unwrap_or(i64::MAX),
            })
            .collect()
    }

    pub(crate) fn disks_list(&self) -> Result<Value, ApiFault> {
        Ok(serde_json::to_value(self.disks()).expect("disks are JSON"))
    }

    /// Whether an install is up.
    pub fn building(&self) -> bool {
        self.install.building()
    }

    /// Make a disk and boot the installer at it, as a job that returns as
    /// soon as the installer is up.
    pub(crate) fn disks_build(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let name = &payload["name"];
        let clean = py::text_or_empty(name).trim().to_owned();
        if !a_name(&clean) {
            return Err(bad_name(name));
        }
        let target = self.paths.disks.join(format!("{clean}{SUFFIX}"));
        if SUFFIXES
            .iter()
            .any(|suffix| self.paths.disks.join(format!("{clean}{suffix}")).exists())
        {
            return Err(ApiFault::new(
                "disk-exists",
                format!("There is already a disk called {clean}."),
            ));
        }
        // Unattended, for a machine being made. Windows is answered and
        // switches itself off once generalized (`windows_answers`). Any
        // other disc is clicked through and ends at its first reboot.
        let unattended = py::truthy(&payload["unattended"]);
        let mut plan = DiskBuild {
            source: py::text_or_empty(&payload["source"]),
            target: target.to_string_lossy().into_owned(),
            size_gb: py::int_or_zero(&payload["size_gb"])?,
            network: py::truthy(&payload["network"]),
            ..DiskBuild::default()
        };
        let language = unattended
            .then(|| machine::windows_installer_language(&plan.source))
            .flatten();
        plan.ends_at_reboot = unattended && language.is_none();
        // Made and started by the job, so it happens even if no window is
        // open any more.
        let made = match &payload["machine"] {
            Value::Object(asked) => {
                let mut asked = asked.clone();
                for key in ["type", "name", "cpu", "memory", "settings", "uses"] {
                    asked.entry(key).or_insert(Value::Null);
                }
                // Install with the machine's memory: 4 GB for the install
                // beside a 2 GB machine got OOM-killed mid Windows Setup.
                if let Some(memory) = asked["memory"].as_f64().filter(|mib| *mib >= 1024.0) {
                    plan.memory_mb = memory as i64;
                }
                let mut settings = py::mapping(&asked["settings"])?;
                settings.insert("disk".into(), Value::String(plan.target.clone()));
                asked.insert("settings".into(), Value::Object(settings));
                Some(self.sandbox_config(&asked)?)
            }
            _ => None,
        };
        let runtime = Arc::clone(&self.runtime);
        let installer = self.installer.clone();
        let qemu = (installer.qemu)();
        let display = (installer.display)(&qemu);
        let checked = DiskBuild {
            answers: language.clone().unwrap_or_default(),
            ..plan.clone()
        };
        if let Some(problem) = checked.validate(&qemu, &display).into_iter().next() {
            return Err(ApiFault::because(
                "bad-disk",
                "This disk cannot be made",
                Some(problem),
            ));
        }

        let install = Arc::clone(&self.install);
        let temp = self.paths.temp.clone();
        let job = run(&self.jobs, KIND, move |say| {
            // Where QEMU listens: sockets here on Linux, ports on 127.0.0.1
            // on Windows, recorded in these same two files (endpoint).
            let channels = room_in(&temp).and_then(|room| {
                let monitor = endpoint::prepare(&room.join("monitor.sock"));
                let agent = endpoint::prepare(&room.join("agent.sock"));
                match (monitor, agent) {
                    (Ok(monitor), Ok(agent)) => Ok((room, monitor, agent)),
                    (Err(error), _) | (_, Err(error)) => {
                        let _ = fs::remove_dir_all(&room);
                        Err(error)
                    }
                }
            });
            let room = match channels {
                Ok((room, monitor, agent)) => {
                    plan.monitor_socket = monitor;
                    plan.agent_socket = agent;
                    room
                }
                // Said as the Python build said it: `FileNotFoundError: ...`.
                Err(error) => {
                    return (false, Said::Text(format!("{}: {error}", io_kind(&error))));
                }
            };
            if let Some(language) = &language {
                let answers = room.join("answers");
                let written = fs::create_dir(&answers)
                    .and_then(|()| {
                        fs::write(answers.join("autounattend.xml"), windows_answers(language))
                    })
                    .and_then(|()| {
                        fs::write(answers.join(MACHINE_ANSWERS), WINDOWS_MACHINE_ANSWERS)
                    });
                if let Err(error) = written {
                    let _ = fs::remove_dir_all(&room);
                    return (false, Said::Text(format!("{}: {error}", io_kind(&error))));
                }
                plan.answers = answers.to_string_lossy().into_owned();
            }
            let prepared = (|| -> Result<Box<dyn Guest>, MachineError> {
                let argv = machine::prepare_install(
                    &plan,
                    &qemu,
                    &display,
                    installer.runner.as_ref(),
                    &(installer.uuid)(),
                )?;
                installer.launch.launch(&argv).map_err(|error| {
                    MachineError::new(reason!(
                        "machine.could_not_start",
                        binary = QEMU_BINARY,
                        detail = error,
                    ))
                })
            })();
            let guest = match prepared {
                Ok(guest) => guest,
                Err(error) => {
                    // An empty disk left behind would block the next try at
                    // the same name. Unless the refusal was that the target
                    // exists: then it isn't ours to delete.
                    if error
                        .reasons
                        .iter()
                        .all(|why| why.code != "build.target_exists")
                    {
                        let _ = fs::remove_file(&plan.target);
                    }
                    let _ = fs::remove_dir_all(&room);
                    return (
                        false,
                        error
                            .first()
                            .cloned()
                            .map_or_else(|| "the disk could not be made".into(), Said::Reason),
                    );
                }
            };
            let guest: Shared = Arc::new(Mutex::new(guest));
            let done = Arc::new(Flag::default());
            *install.done.lock().unwrap_or_else(PoisonError::into_inner) = Arc::clone(&done);
            *install
                .running
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&guest));
            watch(say, &guest, &done, &plan, &installer);
            *install
                .running
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
            // Gone unasked and not by switching off: a boot record alone
            // isn't a finished install, and made a machine that won't start.
            let asked = done.wait(Duration::ZERO);
            let fell = !asked
                && !guest
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .ended_well();
            let ended = if fell {
                let _ = fs::remove_file(&plan.target);
                (false, Said::Reason(reason!("disks.install_killed")))
            } else {
                installed(
                    &plan.target,
                    &qemu,
                    installer.runner.as_ref(),
                    &room.join(".head"),
                )
            };
            let _ = fs::remove_dir_all(&room);
            match (ended, made) {
                ((true, _), Some(config)) => make_machine(&runtime, config),
                (ended, _) => ended,
            }
        })?;
        Ok(crate::jobs::wire(&job))
    }

    /// Delete a disk BeatBox made, and answer with what is left.
    ///
    /// Only inside `disks/`, and the name is checked, not trusted: a path
    /// taken for a name here deletes somebody's work.
    pub(crate) fn disks_forget(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let name = &payload["name"];
        let clean = py::text_or_empty(name).trim().to_owned();
        if !a_name(&clean) {
            return Err(bad_name(name));
        }
        // The entry, not what it points at: a disk linked from another
        // drive loses the link and keeps the file.
        let target = SUFFIXES
            .iter()
            .map(|suffix| self.paths.disks.join(format!("{clean}{suffix}")))
            .find(|candidate| {
                fs::symlink_metadata(candidate)
                    .is_ok_and(|meta| meta.is_file() || meta.file_type().is_symlink())
            })
            .unwrap_or_default();
        if target.as_os_str().is_empty() {
            return Err(ApiFault::because(
                "no-disk",
                format!("There is no disk called {clean}."),
                Some(reason!("disk.unknown", name = clean)),
            ));
        }
        // A machine's overlay may be backed by this. Deleting it doesn't
        // stop the machine, it fails every uncached read. Compared as
        // files, not text: TEMP and Temp are one folder on Windows.
        for sandbox in self.runtime.sandboxes() {
            let disk = sandbox
                .config()
                .setting("disk")
                .map(py::text)
                .unwrap_or_default();
            if same_file(&disk, &target) {
                return Err(ApiFault::because(
                    "disk-in-use",
                    format!("{} is using {clean}.", sandbox.name()),
                    Some(reason!(
                        "disk.in_use",
                        name = clean,
                        machine = sandbox.name()
                    )),
                ));
            }
        }
        fs::remove_file(&target)
            .map_err(|error| ApiFault::new("internal", format!("{}: {error}", io_kind(&error))))?;
        self.disks_list()
    }

    /// Say the install is done: shut the guest down and let the job end.
    pub(crate) fn disks_finish(&self) -> Result<Value, ApiFault> {
        let nothing = || ApiFault::new("no-install", "No install is running.");
        if !self.building() {
            return Err(nothing());
        }
        self.install
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set();
        let current = self.jobs.current().ok_or_else(nothing)?;
        Ok(crate::jobs::wire(&current))
    }
}

/// An I/O error's kind, named the way the Python build named it.
fn io_kind(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        _ => "OSError",
    }
}

/// The machine an install was for, made and started, answering with its id.
/// A refused start (not enough memory, say) leaves it made and off.
fn make_machine(runtime: &SandboxRuntime, config: SandboxConfig) -> (bool, Said) {
    match runtime.create(config) {
        Ok(sandbox) => {
            if let Err(error) = sandbox.apply(Transition::Start) {
                log::warn!("{} was made but did not start: {error}", sandbox.name());
            }
            (true, Said::Text(sandbox.id().to_owned()))
        }
        Err(EngineError::Refused(why)) => (false, Said::Reason(why)),
        Err(EngineError::Provider(error)) => (
            false,
            error.reason.map_or_else(
                || Said::Text("the machine could not be made".into()),
                Said::Reason,
            ),
        ),
        Err(error) => (false, Said::Text(error.to_string())),
    }
}

/// The answers a machine made from a Windows disk boots with, on the answers
/// drive beside `autounattend.xml` and copied onto the disk before sysprep.
pub const MACHINE_ANSWERS: &str = "machine.xml";

/// What each machine made from a generalized Windows disk is answered: only
/// BypassNRO, so Windows 11 makes a local account offline. Everything else --
/// region, keyboard, the account -- Windows asks, in the machine's window.
pub const WINDOWS_MACHINE_ANSWERS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
  <settings pass="specialize">
    <component name="Microsoft-Windows-Deployment" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add">
          <Order>1</Order>
          <Path>reg add HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\OOBE /v BypassNRO /t REG_DWORD /d 1 /f</Path>
        </RunSynchronousCommand>
      </RunSynchronous>
    </component>
  </settings>
</unattend>
"#;

/// Windows Setup's answers for an install nobody watches, ending in a disk
/// every machine is made from: Microsoft's own way of making one.
///
/// Answered: language, one active BIOS partition, Microsoft's generic Pro
/// key (picks the edition, activates nothing), the licence. LabConfig skips
/// Windows 11's TPM and Secure Boot checks; Windows 10 ignores it. Then audit
/// mode copies [`WINDOWS_MACHINE_ANSWERS`] onto the disk and sysprep
/// generalizes it and switches it off, so each machine gets its own identity.
///
/// Don't end at the first reboot: Windows 11 24H2 Setup reboots halfway
/// through copying, and stopping there leaves a disk that won't boot.
pub fn windows_answers(language: &str) -> String {
    const ANSWERS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-International-Core-WinPE" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <SetupUILanguage>
        <UILanguage>LANGUAGE</UILanguage>
      </SetupUILanguage>
      <InputLocale>LANGUAGE</InputLocale>
      <SystemLocale>LANGUAGE</SystemLocale>
      <UILanguage>LANGUAGE</UILanguage>
      <UserLocale>LANGUAGE</UserLocale>
    </component>
    <component name="Microsoft-Windows-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add">
          <Order>1</Order>
          <Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassTPMCheck /t REG_DWORD /d 1 /f</Path>
        </RunSynchronousCommand>
        <RunSynchronousCommand wcm:action="add">
          <Order>2</Order>
          <Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassSecureBootCheck /t REG_DWORD /d 1 /f</Path>
        </RunSynchronousCommand>
        <RunSynchronousCommand wcm:action="add">
          <Order>3</Order>
          <Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassRAMCheck /t REG_DWORD /d 1 /f</Path>
        </RunSynchronousCommand>
      </RunSynchronous>
      <DiskConfiguration>
        <Disk wcm:action="add">
          <DiskID>0</DiskID>
          <WillWipeDisk>true</WillWipeDisk>
          <CreatePartitions>
            <CreatePartition wcm:action="add">
              <Order>1</Order>
              <Type>Primary</Type>
              <Extend>true</Extend>
            </CreatePartition>
          </CreatePartitions>
          <ModifyPartitions>
            <ModifyPartition wcm:action="add">
              <Order>1</Order>
              <PartitionID>1</PartitionID>
              <Active>true</Active>
              <Format>NTFS</Format>
              <Label>Windows</Label>
              <Letter>C</Letter>
            </ModifyPartition>
          </ModifyPartitions>
        </Disk>
      </DiskConfiguration>
      <ImageInstall>
        <OSImage>
          <InstallTo>
            <DiskID>0</DiskID>
            <PartitionID>1</PartitionID>
          </InstallTo>
        </OSImage>
      </ImageInstall>
      <UserData>
        <ProductKey>
          <Key>VK7JG-NPHTM-C97JM-9MPGT-3V66T</Key>
          <WillShowUI>OnError</WillShowUI>
        </ProductKey>
        <AcceptEula>true</AcceptEula>
      </UserData>
    </component>
  </settings>
  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-Deployment" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <Reseal>
        <Mode>Audit</Mode>
      </Reseal>
    </component>
  </settings>
  <settings pass="auditUser">
    <component name="Microsoft-Windows-Deployment" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add">
          <Order>1</Order>
          <Path>cmd /c for %d in (D E F G H I J K L M N O P) do @if exist %d:\MACHINE copy /y %d:\MACHINE C:\Windows\Panther\MACHINE</Path>
        </RunSynchronousCommand>
        <RunSynchronousCommand wcm:action="add">
          <Order>2</Order>
          <Path>C:\Windows\System32\Sysprep\sysprep.exe /generalize /oobe /shutdown /quiet /unattend:C:\Windows\Panther\MACHINE</Path>
        </RunSynchronousCommand>
      </RunSynchronous>
    </component>
  </settings>
</unattend>
"#;
    ANSWERS
        .replace("LANGUAGE", language)
        .replace("MACHINE", MACHINE_ANSWERS)
}

/// A fresh private `beatbox-install-*` folder under `temp`.
fn room_in(temp: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(temp)?;
    loop {
        let id: String = beatbox_isolation::uuid4()
            .replace('-', "")
            .chars()
            .take(8)
            .collect();
        let room = temp.join(format!("beatbox-install-{id}"));
        match fs::create_dir(&room) {
            Ok(()) => return Ok(room),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Wait for whichever comes first: the guest ending itself, or somebody
/// saying it is done. A Linux install that powers off at the end takes the
/// first; a Windows reboots into itself and takes the second.
fn watch(say: Say<'_>, guest: &Shared, done: &Flag, plan: &DiskBuild, installer: &Installer) {
    say(WORKING);
    let mut answered = false;
    let mut listened = Duration::ZERO;
    while alive(guest) && !done.wait(TICK) {
        listened += TICK;
        if answered || listened < installer.listen_every {
            continue;
        }
        listened = Duration::ZERO;
        if answers(&plan.agent_socket) {
            answered = true;
            say(READY);
        }
    }
    if alive(guest) {
        say(FINISHING);
        shut_down(guest, &plan.monitor_socket, installer.grace);
    }
    while alive(guest) {
        thread::sleep(Duration::from_millis(100));
    }
}

/// Whether qemu-guest-agent answers yet. Quiet on failure: not answering is
/// normal for almost all of an install.
fn answers(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let Ok(mut agent) = GuestAgent::connect(Path::new(path), Duration::from_secs(1)) else {
        return false;
    };
    let answered = agent.sync(Duration::from_secs(1));
    agent.close();
    answered
}

/// Ask, wait, then insist. Pulling the plug leaves the master mid-write, and
/// every machine copied from it inherits an unclosed filesystem.
fn shut_down(guest: &Shared, monitor: &str, grace: Duration) {
    let stop = || {
        guest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stop(Duration::from_secs(10));
    };
    let Ok(mut monitor) = Monitor::connect(Path::new(monitor), Duration::from_secs(5)) else {
        log::warn!("BeatBox could not reach the install's monitor; stopping it outright");
        stop();
        return;
    };
    if let Err(error) = monitor.execute("system_powerdown", None) {
        log::error!("BeatBox could not ask the install to shut down: {error}");
    }
    monitor.close();

    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !alive(guest) {
            return;
        }
        thread::sleep(TICK);
    }
    log::warn!(
        "BeatBox waited {:.0}s for the install to shut down; stopping it",
        grace.as_secs_f64()
    );
    stop();
}

/// Same file on disk, or, if either is missing, the same resolved path.
fn same_file(given: &str, target: &Path) -> bool {
    if given.is_empty() {
        return false;
    }
    let given = Path::new(given);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (fs::metadata(given), fs::metadata(target)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    let normal = |path: &Path| {
        let real = crate::leftovers::resolved(path);
        let text = real.to_string_lossy().into_owned();
        if host_os::WINDOWS {
            text.to_lowercase().replace('/', "\\")
        } else {
            text
        }
    };
    normal(given) == normal(target)
}
