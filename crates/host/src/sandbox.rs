//! The first real sandbox: a machine with its own kernel.
//!
//! One QEMU guest booted from the user's disk, writing to an overlay BeatBox
//! makes and deletes. Two channels: the monitor for what QEMU can do to the
//! guest, the agent for what only something inside can do. Most of the
//! design is the difference between them.
//!
//! **Isolation is `machine`, honestly:** its own kernel, no shared folders,
//! clipboard or passthrough, and no network card unless asked.
//!
//! **BeatBox never fetches a disk.** `os` only describes what's on it.
//!
//! **Capabilities narrow to what this guest can do.** No agent channel, no
//! `process.execute`, so the runtime refuses before this module is touched.

use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use beatbox_engine::runtime::capabilities::CapabilitySet;
use beatbox_engine::runtime::provider::unsupported;
use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{
    Action, ActionSpec, Data, Emitter, Field, Isolation, Kind, Observation, ProviderError,
    ProviderSpec, Resources, SandboxConfig, SandboxIdentity, SandboxProvider, data,
};
use beatbox_isolation::base64;
use beatbox_isolation::capture::read_capture;

use crate::probe;
use beatbox_isolation::endpoint;
use beatbox_isolation::firmware::{BIOS, UEFI, disk_start, uefi_install_hint};
use beatbox_isolation::machine::{
    Cable, MachineError, MachineSpec, NET_ATTACHED, NET_NONE, NETWORK_MODES, RunningMachine,
    create_overlay, disk_snapshots, is_optical, start_machine,
};
use beatbox_isolation::qmp::{ControlError, Monitor};
use beatbox_reasons::{Reason, reason};
use serde_json::{Map, Value, json};

use crate::environment::{Agent, Environment};
use crate::settings::{flag, py_int, py_str, text, truthy};

/// How long to wait for QEMU's own control channel after starting it. The
/// socket appears as QEMU initialises, long before anything boots.
pub const MONITOR_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait for the agent. Short: most disks have none, and that's
/// fewer capabilities, not a failure.
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a reattach may wait, tried once per sandbox. The socket exists,
/// so something listens or it doesn't, and a screen polling every few
/// seconds mustn't stall on a sandbox that's gone.
pub const RECONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// How long each late ask of the agent may take. See
/// [`crate::environment::AGENT_RETRY_EVERY`].
pub const AGENT_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// How long an observation waits for QEMU to have pushed anything. Events
/// arrive on the monitor only while somebody is reading it, so a shutdown the
/// guest chose surfaces on the next observation.
pub const EVENT_POLL: Duration = Duration::from_millis(50);

/// For a guest nobody sized: two cores and 2 GiB boot every desktop
/// distribution made this decade and fit on a laptop also running this.
pub const DEFAULT_CPUS: i64 = 2;
/// See [`DEFAULT_CPUS`].
pub const DEFAULT_MEMORY_MB: i64 = 2048;

/// What QEMU itself takes beyond the guest's memory. A rough figure, erring
/// towards saying no.
pub const QEMU_OVERHEAD_MB: i64 = 512;

/// What the agent is asked to run for `process.list`. Linux-only, which is
/// why the capability is withheld on any other family.
pub const PS_ARGUMENTS: [&str; 3] = ["-eo", "pid,ppid,user:20,stat,comm", "--no-headers"];

/// What the agent is asked to run for `process.list` on Windows: every
/// process, with its user, as CSV without a header.
pub const TASKLIST: &str = "C:\\Windows\\System32\\tasklist.exe";
/// Windows PowerShell, which every Windows since 7 has in the same place.
pub const POWERSHELL: &str = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
/// The longest `process.shell` waits for a line to finish.
pub const SHELL_TIMEOUT_MAX: f64 = 300.0;
/// See [`TASKLIST`].
pub const TASKLIST_ARGUMENTS: [&str; 4] = ["/v", "/fo", "csv", "/nh"];
/// What ends a process for `process.kill`, at once: something that is being
/// watched in a sandbox is not asked politely.
pub const KILL_LINUX: &str = "/bin/kill";
/// See [`KILL_LINUX`].
pub const KILL_WINDOWS: &str = "C:\\Windows\\System32\\taskkill.exe";

/// The most a pen can hold. QEMU's `vvfat` builds a FAT16 disk of 516 MB
/// around the folder, file system included, and refuses anything more with
/// "Directory does not fit"; this leaves the file system its room and says
/// so before QEMU has to.
pub const PEN_LIMIT_BYTES: u64 = 500 * 1024 * 1024;
/// What the pen is called inside the guest. FAT labels are 11 characters.
pub const PEN_LABEL: &str = "BEATBOX PEN";

/// This machine's own UEFI variables, in its room.
pub const EFI_VARS: &str = "efivars.qcow2";

/// What a capture asked for with `capture: true` is called, in the room.
pub const TRAFFIC: &str = "traffic.pcap";

/// Whether an address is somewhere outside this computer and its sandbox
/// network: not private (QEMU's own 10.0.2.0/24 included), loopback,
/// link-local, multicast or broadcast. `ARP`'s "broadcast" is not an address.
pub fn is_outside(address: &str) -> bool {
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_unspecified())
        }
        Ok(IpAddr::V6(ip)) => {
            !(ip.is_loopback()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_unicast_link_local()
                || ip.is_unique_local())
        }
        Err(_) => false,
    }
}

/// How long a stop waits for QEMU to go before it insists.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Beside the overlay: the hardware each save point in it was taken on.
const SAVE_POINTS: &str = "save-points.json";

/// Beside the overlay: the guest's screen as it last was, a PNG.
const SCREEN: &str = "screen.png";

/// The save point `snapshot.save_state` leaves in the overlay: the machine
/// as it was switched off, put back and forgotten by the next start. Not one
/// of [`HostSandbox::snapshots`], and no name a person may take.
const SAVED_STATE: &str = "beatbox-saved-state";

/// Everything this type can ever do.
pub const CAPABILITIES: &[&str] = &[
    "lifecycle.pause",
    "lifecycle.reset",
    "lifecycle.reconfigure",
    "snapshot.create",
    "snapshot.restore",
    "snapshot.delete",
    "snapshot.list",
    "snapshot.save_state",
    "process.execute",
    "process.shell",
    "process.list",
    "process.kill",
    "filesystem.read",
    "filesystem.write",
    "network.interfaces",
    "network.traffic",
    "network.probe",
    "media.insert",
    "media.pen",
    "media.eject",
    "display.picture",
];

/// What each capability past the lifecycle takes and gives back. The
/// runtime checks the inputs; the tests check the outputs.
pub fn actions() -> Vec<ActionSpec> {
    use Kind::{Bool, List, Number, Text};
    let need = Field::required;
    let may = Field::optional;
    let ran = || {
        vec![
            need("pid", Number),
            need("exit_code", Number),
            need("stdout", Text),
            need("stderr", Text),
            need("truncated", Bool),
        ]
    };
    let named = |capability: &str| {
        ActionSpec::new(
            capability,
            vec![need("name", Text)],
            vec![need("name", Text)],
        )
    };
    let mut shell = ran();
    shell.push(need("shell", Text));
    vec![
        ActionSpec::new(
            "process.execute",
            vec![
                need("command", Text),
                may("arguments", List),
                may("timeout", Number),
                may("conceal", List),
            ],
            ran(),
        ),
        ActionSpec::new(
            "process.shell",
            vec![need("line", Text), may("timeout", Number)],
            shell,
        ),
        ActionSpec::new("process.list", vec![], vec![need("processes", List)]),
        ActionSpec::new(
            "process.kill",
            vec![need("pid", Number)],
            vec![need("pid", Number)],
        ),
        ActionSpec::new(
            "filesystem.read",
            vec![need("path", Text)],
            vec![
                need("path", Text),
                need("content", Text),
                need("bytes", Number),
            ],
        ),
        ActionSpec::new(
            "filesystem.write",
            vec![need("path", Text), may("content", Text)],
            vec![need("path", Text), need("bytes", Number)],
        ),
        ActionSpec::new("network.interfaces", vec![], vec![need("interfaces", List)]),
        ActionSpec::new(
            "network.probe",
            vec![
                need("kind", Text),
                need("address", Text),
                need("port", Number),
                may("name", Text),
                may("username", Text),
                may("database", Text),
                may("password", Text),
            ],
            vec![
                need("ok", Bool),
                need("ms", Number),
                may("said", Text),
                may("answer", Text),
            ],
        ),
        ActionSpec::new(
            "network.traffic",
            vec![],
            vec![
                need("recording", Bool),
                need("file", Text),
                need("size", Number),
                need("packets", Number),
                need("outside", List),
                need("local", Number),
                need("names", List),
            ],
        ),
        named("snapshot.create"),
        named("snapshot.restore"),
        named("snapshot.delete"),
        ActionSpec::new("snapshot.list", vec![], vec![need("snapshots", List)]),
        ActionSpec::new("snapshot.save_state", vec![], vec![]),
        ActionSpec::new(
            "media.insert",
            vec![need("folder", Text), may("label", Text)],
            vec![
                need("folder", Text),
                need("label", Text),
                need("already", Bool),
            ],
        ),
        ActionSpec::new(
            "media.pen",
            vec![need("folder", Text)],
            vec![
                need("folder", Text),
                need("label", Text),
                need("bytes", Number),
            ],
        ),
        ActionSpec::new("media.eject", vec![], vec![]),
        ActionSpec::new(
            "display.picture",
            vec![],
            vec![need("picture", Text), need("taken", Number)],
        ),
    ]
}

/// Everything that needs something running inside the guest to answer.
const NEEDS_AGENT: [&str; 8] = [
    "process.execute",
    "process.shell",
    "process.list",
    "process.kill",
    "filesystem.read",
    "filesystem.write",
    "network.interfaces",
    "network.probe",
];

/// Everything that needs a disk of the sandbox's own to write into.
const NEEDS_DISK: [&str; 5] = [
    "snapshot.create",
    "snapshot.restore",
    "snapshot.delete",
    "snapshot.list",
    "snapshot.save_state",
];

/// What a configuration may not change once the sandbox exists. The overlay
/// is backed by `disk` and lives in `workspace`; a different either is a
/// different machine.
const FIXED: [&str; 2] = ["disk", "workspace"];

/// A QEMU guest, as a sandbox.
///
/// Configured through `settings`:
///
/// | | |
/// |---|---|
/// | `disk` | the image to boot. Required, and never fetched. |
/// | `os` | `{family, distribution, version}` -- a description of what is on that disk. |
/// | `network` | `none` (the default), `isolated` or `nat` -- or what a network sandbox's `network.attach` answered, which cables the card to it. |
/// | `agent` | whether to open a guest-agent channel at all. Default true. |
/// | `display` | whether it gets a window of its own. Default false. |
/// | `hostile` | what runs here is not trusted. Forbids things; relaxes nothing. |
/// | `capture` | `true`: every packet, to `traffic.pcap` in the workspace, gone with it. A path: to that path. |
/// | `ports` | `[[host, guest], ...]`, each one a deliberate hole. |
/// | `workspace` | where the overlay and the sockets live. |
///
/// Sized through the configuration's resources rather than its settings,
/// because the runtime keeps a ledger of those: `cpu` becomes `-smp`,
/// `memory` becomes `-m`, and the runtime never learns what either means.
pub struct HostSandbox {
    identity: SandboxIdentity,
    config: SandboxConfig,
    emitter: Emitter,
    environment: Environment,
    disk: String,
    os: Map<String, Value>,
    wants_agent: bool,
    workspace: PathBuf,
    optical: bool,
    overlay: PathBuf,
    monitor_socket: PathBuf,
    agent_socket: PathBuf,
    display_socket: PathBuf,
    display_websocket: PathBuf,
    display_password: PathBuf,
    machine: Option<RunningMachine>,
    reattach_failed: bool,
    monitor: Option<Monitor>,
    agent: Option<Box<dyn Agent>>,
    agent_asked: Option<Instant>,
    snapshots: Vec<String>,
    /// Whether the overlay holds [`SAVED_STATE`] for the next start.
    saved_state: bool,
    /// Why the last start could not carry on from the saved state and
    /// booted afresh instead; said until it stops.
    state_lost: Option<Reason>,
    /// For each save point, the hardware it was taken on ([`Self::setup_now`]).
    /// Written beside the overlay; a save point older than this record has
    /// none, and is tried without the check.
    setups: Map<String, Value>,
    /// Why the guest is about to be found switched off, when it was this
    /// sandbox that switched it off in the middle of an action.
    halted_because: Option<Reason>,
    /// The folder plugged in as a pen, while it is.
    pen: Option<String>,
    /// The folder and label plugged in by `media.insert` (the guest tools
    /// drive), while it is. Hardware too, as far as a saved state cares.
    media: Option<(String, String)>,
    /// What the guest said it is, asked once per run.
    guest_family: Option<String>,
    /// How the disk starts -- its firmware, and whether anything on it can
    /// start at all -- read off it once.
    detected_start: std::sync::OnceLock<(&'static str, bool)>,
}

impl HostSandbox {
    /// `host`, version 1, isolating at the machine level, on this computer.
    pub fn spec() -> ProviderSpec {
        Self::spec_with(Environment::here())
    }

    /// The same type, asking `environment` instead of this computer.
    pub fn spec_with(environment: Environment) -> ProviderSpec {
        ProviderSpec::new(
            "host",
            "1",
            CAPABILITIES,
            Isolation::Machine,
            move |identity, config, emit| {
                Box::new(Self::new(identity, config, emit, environment.clone()))
            },
        )
        .with_actions(&actions())
    }

    /// One host sandbox, for one sandbox.
    pub fn new(
        identity: SandboxIdentity,
        config: SandboxConfig,
        emitter: Emitter,
        environment: Environment,
    ) -> Self {
        let disk = text(&config, "disk");
        let os = config
            .setting("os")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let wants_agent = flag(&config, "agent", true);
        let workspace = match text(&config, "workspace") {
            room if room.is_empty() => {
                std::env::temp_dir().join(format!("beatbox-host-{}", identity.id))
            }
            room => PathBuf::from(room),
        };
        // A disc is read-only and has no overlay to write into, so it runs
        // the disposable way and cannot be snapshotted. Decided once, here,
        // because four capabilities and the whole lifecycle depend on it.
        let optical = !disk.is_empty() && is_optical(&disk);
        let overlay = workspace.join("disk.qcow2");
        // Read off the disk, not remembered: a sandbox restored after BeatBox
        // was closed has the same overlay and the same save points in it.
        let mut snapshots = if !optical && overlay.is_file() {
            let qemu = (environment.qemu)();
            disk_snapshots(
                &overlay.to_string_lossy(),
                &qemu,
                environment.runner.as_ref(),
            )
        } else {
            Vec::new()
        };
        let setups = read_setups(&workspace.join(SAVE_POINTS), &snapshots);
        let saved_state = snapshots.iter().any(|name| name == SAVED_STATE);
        snapshots.retain(|name| name != SAVED_STATE);
        Self {
            monitor_socket: workspace.join("monitor.sock"),
            agent_socket: workspace.join("agent.sock"),
            display_socket: workspace.join("display.sock"),
            display_websocket: workspace.join("display.websocket"),
            display_password: workspace.join("display.password"),
            identity,
            config,
            emitter,
            environment,
            disk,
            os,
            wants_agent,
            workspace,
            optical,
            overlay,
            machine: None,
            reattach_failed: false,
            monitor: None,
            agent: None,
            agent_asked: None,
            snapshots,
            setups,
            saved_state,
            state_lost: None,
            halted_because: None,
            pen: None,
            media: None,
            guest_family: None,
            detected_start: std::sync::OnceLock::new(),
        }
    }

    // --- exposed for tests and tooling; nothing in the engine reaches through ---

    /// The whole configuration, turned into something QEMU understands --
    /// resolved, not prepared: a validation must not pick ports.
    pub fn machine_spec(&self) -> Result<MachineSpec, ProviderError> {
        self.spec_of(false)
    }

    /// What this computer is, to this sandbox. Tests swap pieces of it.
    pub fn environment_mut(&mut self) -> &mut Environment {
        &mut self.environment
    }

    /// The QEMU process this sandbox started, if it did.
    pub fn machine_mut(&mut self) -> Option<&mut RunningMachine> {
        self.machine.as_mut()
    }

    /// The monitor, if one is open.
    pub fn monitor_mut(&mut self) -> Option<&mut Monitor> {
        self.monitor.as_mut()
    }

    /// Where QEMU's monitor listens, or where the file naming it is.
    pub fn monitor_socket(&self) -> &Path {
        &self.monitor_socket
    }

    /// Put an agent in, as if one had answered.
    pub fn set_agent(&mut self, agent: Box<dyn Agent>) {
        self.agent = Some(agent);
    }

    /// Whether an agent is held.
    pub fn has_agent(&self) -> bool {
        self.agent.is_some()
    }

    /// What the guest says about itself, as an observation reports it.
    pub fn guest_facts(&mut self) -> Data {
        let Some(agent) = self.agent.as_mut() else {
            return data!({"agent": false});
        };
        match agent.interfaces() {
            Ok(interfaces) => {
                let addresses: Vec<Value> = interfaces
                    .iter()
                    .flat_map(addresses_of)
                    .filter_map(|address| {
                        address
                            .get("ip-address")
                            .filter(|a| a.as_str().is_some_and(|s| !s.is_empty()))
                            .cloned()
                    })
                    .collect();
                let mut facts = data!({"addresses": addresses});
                // Which shell a line goes through (`process.shell`), said up
                // front so a page can offer the right one: asked once a run.
                if let Ok(family) = self.guest_family("network.interfaces") {
                    facts.insert("family".into(), json!(family));
                }
                facts
            }
            Err(_) => {
                self.lose_agent();
                data!({"agent": false})
            }
        }
    }

    /// A broken channel is dropped; a refusal *through* a working one is not.
    pub fn agent_failed(&mut self, error: &ControlError) {
        if matches!(
            error.code(),
            "machine.channel_gone" | "machine.channel_noise"
        ) {
            self.lose_agent();
        }
    }

    /// Close both channels and forget them, as a process exiting would.
    pub fn forget_channels(&mut self) {
        if let Some(mut monitor) = self.monitor.take() {
            monitor.close();
        }
        if let Some(mut agent) = self.agent.take() {
            agent.close();
        }
    }

    // --- the machinery --------------------------------------------------------

    fn emit(&self, name: &str, data: Data) {
        if let Err(error) = self.emitter.emit(name, data) {
            log::error!(
                "BeatBox host sandbox {} could not say {name}: {error}",
                self.identity.id
            );
        }
    }

    fn family(&self) -> String {
        match self.os.get("family") {
            Some(value) if truthy(value) => py_str(value).to_lowercase(),
            _ => "linux".into(),
        }
    }

    /// `bios` or `uefi`: what `firmware` says, or -- unset, or `auto` --
    /// what the disk was installed for, read off its partition table once.
    /// A disc is BIOS unless asked: every installer disc boots under it.
    pub fn firmware(&self) -> &'static str {
        match text(&self.config, "firmware").as_str() {
            "uefi" => UEFI,
            "bios" => BIOS,
            _ if self.optical => BIOS,
            _ => self.detected_start().0,
        }
    }

    /// Whether anything on the disk can start. Otherwise QEMU shows iPXE and
    /// "No bootable device", which means nothing to most people, so the
    /// screen says it instead. A disc always can.
    pub fn bootable(&self) -> bool {
        self.optical || self.detected_start().1
    }

    fn detected_start(&self) -> (&'static str, bool) {
        *self.detected_start.get_or_init(|| {
            let qemu = (self.environment.qemu)();
            let runner = self.environment.runner.as_ref();
            let format = beatbox_isolation::machine::disk_format(&self.disk, &qemu, runner)
                .unwrap_or_else(|_| "raw".into());
            disk_start(
                &self.disk,
                &format,
                &qemu,
                runner,
                &self.workspace.join(".head"),
            )
        })
    }

    /// UEFI's code and this machine's variables, or two empty strings for
    /// the BIOS. The variables are made from the firmware's template at the
    /// first start, and are the machine's from then on.
    fn uefi_pair(&self, prepared: bool) -> Result<(String, String), ProviderError> {
        if self.firmware() != UEFI {
            return Ok((String::new(), String::new()));
        }
        let qemu = (self.environment.qemu)();
        let Some(uefi) = (self.environment.uefi)(&qemu) else {
            return Err(ProviderError::new(reason!(
                "host.no_uefi",
                command = uefi_install_hint()
            )));
        };
        let vars = self.workspace.join(EFI_VARS);
        if prepared && !vars.is_file() {
            let argv = vec![
                qemu.image.clone(),
                "convert".into(),
                "-f".into(),
                "raw".into(),
                "-O".into(),
                "qcow2".into(),
                uefi.vars_template.to_string_lossy().into_owned(),
                vars.to_string_lossy().into_owned(),
            ];
            let made = self.environment.runner.run(&argv, None);
            if !made.as_ref().is_ok_and(|output| output.code == 0) {
                let detail = match made {
                    Ok(output) => output.stderr.trim().to_owned(),
                    Err(error) => error.to_string(),
                };
                return Err(ProviderError::new(reason!(
                    "host.no_uefi_vars",
                    detail = detail
                )));
            }
        }
        Ok((
            uefi.code.to_string_lossy().into_owned(),
            vars.to_string_lossy().into_owned(),
        ))
    }

    /// The whole configuration, turned into something QEMU understands. The
    /// one place where generic becomes specific.
    fn spec_of(&self, prepared: bool) -> Result<MachineSpec, ProviderError> {
        let channel = |path: &Path| -> Result<String, ProviderError> {
            if prepared {
                endpoint::prepare(path).map_err(|error| {
                    ProviderError::new(reason!(
                        "machine.could_not_start",
                        binary = "qemu-system-x86_64",
                        detail = error,
                    ))
                })
            } else {
                Ok(endpoint::resolve(path))
            }
        };
        let resources = &self.config.resources;
        let ports: Vec<(i64, i64)> = self
            .config
            .setting("ports")
            .and_then(Value::as_array)
            .map(|pairs| {
                pairs
                    .iter()
                    .filter_map(Value::as_array)
                    .filter(|pair| pair.len() == 2)
                    .map(|pair| (py_int(&pair[0]), py_int(&pair[1])))
                    .collect()
            })
            .unwrap_or_default();
        let cable = match self.config.setting("network") {
            Some(Value::Object(attachment)) => Some(cable(attachment)?),
            _ => None,
        };
        let network = match text(&self.config, "network") {
            _ if cable.is_some() => NET_ATTACHED.to_owned(),
            mode if mode.is_empty() => NET_NONE.to_owned(),
            mode => mode,
        };
        if cable.is_none() && !NETWORK_MODES.contains(&network.as_str()) {
            return Err(ProviderError::new(reason!(
                "machine.bad_network",
                mode = network
            )));
        }
        let memory = resources.get(MEMORY) as i64;
        let cpu = resources.get(CPU);
        let (uefi_code, uefi_vars) = self.uefi_pair(prepared)?;
        let spec = MachineSpec {
            uefi_code,
            uefi_vars,
            name: if self.identity.name.is_empty() {
                self.identity.id.clone()
            } else {
                self.identity.name.clone()
            },
            image: if self.optical {
                self.disk.clone()
            } else {
                self.overlay.to_string_lossy().into_owned()
            },
            memory_mb: if memory == 0 {
                DEFAULT_MEMORY_MB
            } else {
                memory
            },
            cpus: if cpu == 0.0 {
                DEFAULT_CPUS
            } else {
                (cpu.ceil() as i64).max(1)
            },
            network,
            cable,
            headless: !flag(&self.config, "display", false),
            forwarded_ports: ports,
            hostile: flag(&self.config, "hostile", false),
            capture_path: self.capture_path(),
            instrument: flag(&self.config, "instrument", false),
            disposable: self.optical,
            monitor_socket: channel(&self.monitor_socket)?,
            agent_socket: if self.wants_agent {
                channel(&self.agent_socket)?
            } else {
                String::new()
            },
            display_socket: if prepared && !flag(&self.config, "display", false) {
                String::new()
            } else if prepared {
                self.display_socket.to_string_lossy().into_owned()
            } else {
                String::new()
            },
            display_websocket: if prepared && flag(&self.config, "display", false) {
                endpoint::prepare_with(&self.display_websocket, true).map_err(|error| {
                    ProviderError::new(reason!(
                        "machine.could_not_start",
                        binary = "qemu-system-x86_64",
                        detail = error,
                    ))
                })?
            } else {
                String::new()
            },
            display_password_file: if prepared && flag(&self.config, "display", false) {
                self.display_password.to_string_lossy().into_owned()
            } else {
                String::new()
            },
            log_path: self
                .workspace
                .join("qemu.log")
                .to_string_lossy()
                .into_owned(),
        };
        let mut problems = spec.validate();
        // Asked for its screen on a QEMU that has nowhere to draw one: QEMU
        // would refuse to start with a line on stderr nobody reads.
        if !spec.headless {
            let qemu = (self.environment.qemu)();
            if !(self.environment.display_available)(&qemu) {
                let hint = (self.environment.install_hint)();
                problems.push(reason!("build.no_display", install = hint));
            }
        }
        match problems.into_iter().next() {
            Some(first) => Err(ProviderError::new(first)),
            None => Ok(spec),
        }
    }

    /// Pick a guest's control channel back up without having started it.
    /// The monitor answering is the one thing that cannot be faked from
    /// outside, so it is the one thing asked.
    fn reattach(&mut self, timeout: Duration) -> bool {
        match Monitor::connect(&self.monitor_socket, timeout) {
            Ok(monitor) => {
                self.monitor = Some(monitor);
                self.connect_agent(AGENT_TIMEOUT);
                true
            }
            Err(_) => false,
        }
    }

    /// Let go of an agent that stopped answering, and close its connection:
    /// QEMU serves one client at a time, so an unused one keeps the only
    /// slot and the agent is never heard from again.
    fn lose_agent(&mut self) {
        if let Some(mut agent) = self.agent.take() {
            agent.close();
        }
    }

    fn forget_sockets(&self) {
        for path in [
            &self.monitor_socket,
            &self.agent_socket,
            &self.display_socket,
            &self.display_websocket,
            &self.display_password,
        ] {
            let _ = fs::remove_file(path);
        }
    }

    /// Give QEMU a password without putting it in the process list. VNC's
    /// classic authentication uses at most eight characters; the UUID is
    /// freshly generated for this boot and the complete value remains the
    /// guest's SMBIOS identity.
    fn prepare_display_password(&self, uuid: &str) -> Result<(), ProviderError> {
        if !flag(&self.config, "display", false) {
            return Ok(());
        }
        const ALPHABET: &[u8] = b"23456789abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ";
        let mut value = uuid.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
            hash.wrapping_mul(0x100000001b3) ^ u64::from(byte)
        });
        let password: String = (0..8)
            .map(|_| {
                let character = ALPHABET[(value % ALPHABET.len() as u64) as usize] as char;
                value /= ALPHABET.len() as u64;
                character
            })
            .collect();
        fs::write(&self.display_password, password).map_err(|error| {
            ProviderError::new(reason!(
                "machine.could_not_start",
                binary = "qemu-system-x86_64",
                detail = error,
            ))
        })?;
        private_file(&self.display_password);
        Ok(())
    }

    fn display_facts(&self) -> Data {
        if !flag(&self.config, "display", false) {
            return Data::new();
        }
        let endpoint = endpoint::resolve(&self.display_websocket);
        let password = fs::read_to_string(&self.display_password).unwrap_or_default();
        let Some(address) = endpoint.strip_prefix(endpoint::TCP) else {
            return Data::new();
        };
        if password.is_empty() {
            return Data::new();
        }
        data!({
            "display": {
                "url": format!("ws://{address}"),
                "password": password,
            }
        })
    }

    /// Try to reach something inside. Absence is not a failure: most disks
    /// have no agent.
    fn connect_agent(&mut self, timeout: Duration) {
        if !self.wants_agent || self.agent.is_some() {
            return;
        }
        self.agent_asked = Some(Instant::now());
        let Ok(mut agent) = (self.environment.connect_agent)(&self.agent_socket, timeout) else {
            return;
        };
        // sync, not ping: a fresh connection can still find the answer to an
        // earlier client's request waiting in the stream.
        if agent.sync(timeout) {
            self.agent = Some(agent);
            self.emit("guest.agent_ready", Data::new());
        } else {
            agent.close();
        }
    }

    fn require_monitor(&mut self) -> Result<&mut Monitor, ProviderError> {
        // The monitor, not the machine: a reattached guest has a monitor and
        // no process of ours, and is every bit as running.
        let stopped = self.machine.as_mut().is_some_and(|m| !m.running());
        match self.monitor.as_mut() {
            Some(monitor) if !stopped => Ok(monitor),
            _ => Err(ProviderError::new(reason!("engine.not_running"))),
        }
    }

    fn require_agent(&mut self, capability: &str) -> Result<&mut Box<dyn Agent>, ProviderError> {
        self.require_monitor()?;
        if self.agent.is_none() {
            // One more try: the agent starts when the guest has booted, which
            // is minutes after the socket appeared.
            self.connect_agent(AGENT_TIMEOUT);
        }
        self.agent
            .as_mut()
            .ok_or_else(|| ProviderError::new(reason!("host.no_agent", capability = capability)))
    }

    fn required(action: &Action, name: &str) -> Result<String, ProviderError> {
        match action.argument(name) {
            None | Some(Value::Null) => Err(Self::missing(action, name)),
            Some(Value::String(text)) if text.is_empty() => Err(Self::missing(action, name)),
            Some(value) => Ok(py_str(value)),
        }
    }

    fn missing(action: &Action, name: &str) -> ProviderError {
        ProviderError::new(reason!(
            "engine.missing_argument",
            capability = action.capability(),
            argument = name,
        ))
    }

    fn resting_facts(&self) -> Data {
        let overlay = if self.optical {
            String::new()
        } else {
            self.overlay.to_string_lossy().into_owned()
        };
        data!({
            "disk": self.disk,
            "disk_available": Path::new(&self.disk).is_file(),
            "overlay": overlay,
            "os": self.os,
            "agent": self.agent.is_some(),
            "snapshots": self.snapshots,
            "saved_state": self.saved_state,
            "saved_state_lost": self.state_lost.is_some(),
            "pen": self.pen,
            "firmware": self.firmware(),
            "bootable": self.bootable(),
        })
    }

    /// QEMU's own events, under a namespace that says whose they are:
    /// `guest.*`, never `sandbox.*`. Whether a guest that powered itself off
    /// is stopped or failed is the runtime's decision.
    fn report(&self, events: Vec<Value>) {
        for event in events {
            let name = event
                .get("event")
                .map(py_str)
                .unwrap_or_default()
                .to_lowercase();
            if !name.is_empty() {
                let carried = match event.get("data") {
                    Some(value) if truthy(value) => value.clone(),
                    _ => json!({}),
                };
                self.emit(&format!("guest.{name}"), data!({"data": carried}));
            }
        }
    }

    fn kill(&mut self) {
        self.forget_channels();
        if let Some(mut machine) = self.machine.take() {
            machine.stop(STOP_TIMEOUT);
        }
    }

    fn lost(&self, why: Reason) -> Observation {
        Observation {
            alive: Some(false),
            reason: Some(why),
            facts: self.resting_facts(),
            ..Observation::default()
        }
    }

    /// A monitor failure nobody expects, as `engine.provider_failed`.
    fn uncaught(&self, step: &str, error: &ControlError) -> ProviderError {
        ProviderError::new(reason!(
            "engine.provider_failed",
            type = "host",
            step = step,
            detail = format!("ControlError: {}", error.reason),
        ))
    }

    fn with_agent<T>(
        &mut self,
        capability: &str,
        work: impl FnOnce(&mut Box<dyn Agent>) -> Result<T, ControlError>,
    ) -> Result<T, ProviderError> {
        let agent = self.require_agent(capability)?;
        match work(agent) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.agent_failed(&error);
                Err(ProviderError::new(error.reason))
            }
        }
    }

    // --- what it can be asked to do --------------------------------------------

    /// Run one program inside directly, no shell, so no injection. Both
    /// events wait for the agent's answer: otherwise `process.created` goes
    /// out for a program Windows never found.
    fn execute(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let command = Self::required(action, "command")?;
        let arguments: Vec<Value> = match action.argument("arguments") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        };
        let timeout = action
            .argument("timeout")
            .and_then(Value::as_f64)
            .map(Duration::from_secs_f64);
        let agent = self.require_agent("process.execute")?;
        let result = match agent.run(&command, &arguments, timeout) {
            Ok(result) => result,
            Err(error) => {
                self.agent_failed(&error);
                if error.code() == "machine.refused_command" {
                    // The reason names the agent's verb ("guest-exec"), not
                    // the program somebody typed; renamed, with what the guest
                    // actually said kept.
                    let detail = error
                        .reason
                        .values
                        .get("detail")
                        .cloned()
                        .unwrap_or_default();
                    return Err(ProviderError::new(reason!(
                        "host.program_refused",
                        command = command,
                        detail = detail,
                    )));
                }
                return Err(ProviderError::new(error.reason));
            }
        };
        // What the caller said is secret is run, never said: the event goes
        // to the history and to every open page.
        let secrets: Vec<&str> = match action.argument("conceal") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .filter(|secret| !secret.is_empty())
                .collect(),
            _ => Vec::new(),
        };
        self.emit(
            "process.created",
            data!({"command": command, "arguments": concealed(&arguments, &secrets)}),
        );
        self.emit(
            "process.exited",
            data!({"pid": result.get("pid"), "command": command, "exit_code": result.get("exit_code")}),
        );
        Ok(result)
    }

    /// Run one line through the guest's own shell -- `sh -c` on Linux,
    /// PowerShell on Windows. A shell on purpose, unlike `process.execute`:
    /// the line is what somebody typed for their own machine, pipes and all,
    /// and the guest is the boundary, not this call. Waits at most
    /// `timeout` seconds (30 unless said, [`SHELL_TIMEOUT_MAX`] at most).
    fn shell(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let line = Self::required(action, "line")?;
        let windows = self.guest_family("process.shell")? == "windows";
        let (command, words): (&str, Vec<&str>) = if windows {
            (
                POWERSHELL,
                vec!["-NoProfile", "-NonInteractive", "-Command", &line],
            )
        } else {
            ("/bin/sh", vec!["-c", &line])
        };
        let mut arguments = data!({"command": command, "arguments": words});
        if let Some(seconds) = action.argument("timeout").and_then(Value::as_f64) {
            arguments.insert(
                "timeout".into(),
                json!(seconds.clamp(1.0, SHELL_TIMEOUT_MAX)),
            );
        }
        let run = Action::new("process.execute", arguments).expect("a valid capability");
        let mut result = self.execute(&run)?;
        result.insert(
            "shell".into(),
            json!(if windows { "powershell" } else { "sh" }),
        );
        Ok(result)
    }

    /// `linux` or `windows`: what the configuration says, or, when it says
    /// nothing -- a machine made from a disk nobody described -- what the
    /// guest's agent says, asked once per run.
    fn guest_family(&mut self, capability: &str) -> Result<String, ProviderError> {
        if self.os.get("family").is_some_and(truthy) {
            return Ok(self.family());
        }
        if let Some(family) = &self.guest_family {
            return Ok(family.clone());
        }
        let said = self.with_agent(capability, |agent| agent.os())?;
        let family = if said.get("id").and_then(Value::as_str) == Some("mswindows") {
            "windows"
        } else {
            "linux"
        };
        self.guest_family = Some(family.to_owned());
        Ok(family.to_owned())
    }

    fn processes(&mut self) -> Result<Data, ProviderError> {
        let windows = self.guest_family("process.list")? == "windows";
        let (program, words): (&str, &[&str]) = if windows {
            (TASKLIST, &TASKLIST_ARGUMENTS)
        } else {
            ("/bin/ps", &PS_ARGUMENTS)
        };
        let arguments: Vec<Value> = words.iter().map(|a| json!(a)).collect();
        let result =
            self.with_agent("process.list", |agent| agent.run(program, &arguments, None))?;
        let stdout = result
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let processes = if windows {
            parse_tasklist(stdout)
        } else {
            parse_ps(stdout)
        };
        Ok(data!({"processes": processes}))
    }

    /// End one process inside, at once. Its number only, checked to be one:
    /// nothing a caller sends reaches a command line as anything else. Not
    /// 0 or 1 -- the kernel's own, and ending init takes the whole guest.
    fn kill_process(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let pid = match action.argument("pid") {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::String(text)) => text.parse::<i64>().ok(),
            _ => None,
        };
        let Some(pid) = pid.filter(|pid| *pid > 1) else {
            return Err(ProviderError::new(reason!(
                "host.bad_pid",
                pid = action.argument("pid").map(py_str).unwrap_or_default(),
            )));
        };
        let windows = self.guest_family("process.kill")? == "windows";
        let (program, arguments) = if windows {
            (
                KILL_WINDOWS,
                vec![json!("/PID"), json!(pid.to_string()), json!("/F")],
            )
        } else {
            (KILL_LINUX, vec![json!("-KILL"), json!(pid.to_string())])
        };
        let result =
            self.with_agent("process.kill", |agent| agent.run(program, &arguments, None))?;
        let code = result.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            return Err(ProviderError::new(reason!("host.kill_refused", pid = pid)));
        }
        self.emit("process.killed", data!({"pid": pid}));
        Ok(data!({"pid": pid}))
    }

    fn read(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let path = Self::required(action, "path")?;
        let agent = self.require_agent("filesystem.read")?;
        match agent.read_file(&path) {
            Ok(content) => {
                let bytes = content.chars().count();
                Ok(data!({"path": path, "content": content, "bytes": bytes}))
            }
            Err(error) => {
                self.agent_failed(&error);
                // An agent says "no such file" by refusing the open, and a
                // missing file is an ordinary answer rather than a broken
                // channel.
                if error.code() == "machine.refused_command" {
                    return Err(ProviderError::new(reason!(
                        "engine.no_such_file",
                        path = path
                    )));
                }
                Err(ProviderError::new(error.reason))
            }
        }
    }

    fn write(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let path = Self::required(action, "path")?;
        let content = action.argument("content").map(py_str).unwrap_or_default();
        let written = self.with_agent("filesystem.write", |agent| {
            agent.write_file(&path, &content)
        })?;
        // The Linux agent creates files anyone in the guest may change: one
        // a root program then runs is anyone's way to root. What the file
        // already allowed otherwise stays.
        if self.guest_family("filesystem.write")? != "windows" {
            let arguments = [json!("go-w"), json!("--"), json!(path)];
            self.with_agent("filesystem.write", |agent| {
                agent.run("chmod", &arguments, Some(Duration::from_secs(10)))
            })?;
        }
        self.emit("file.written", data!({"path": path, "bytes": written}));
        Ok(data!({"path": path, "bytes": written}))
    }

    /// Where this guest's traffic is written, or empty: `capture: true` is a
    /// file in its own room, which goes when the machine does; a path is that
    /// path. `text()` would read `true` as the string "True".
    fn capture_path(&self) -> String {
        match self.config.setting("capture") {
            Some(Value::Bool(true)) => self.workspace.join(TRAFFIC).to_string_lossy().into_owned(),
            Some(Value::Bool(false)) | None => String::new(),
            Some(_) => text(&self.config, "capture"),
        }
    }

    /// What the guest tried to say, from its capture, for someone who has
    /// never read one: the names it looked up, and where outside this
    /// computer it tried to go. Everything else -- its own address, the
    /// network's gateway and DNS, broadcasts, what every operating system
    /// says to a local network -- is counted, not listed. Read from the file,
    /// so it answers for the last run with the machine stopped, too.
    fn traffic(&self) -> Result<Data, ProviderError> {
        let path = self.capture_path();
        let file = Path::new(&path);
        let size = fs::metadata(file).map(|meta| meta.len()).unwrap_or(0);
        if path.is_empty() || !file.is_file() {
            return Ok(data!({
                "recording": !path.is_empty(), "file": "", "size": 0, "packets": 0,
                "outside": [], "local": 0, "names": [],
            }));
        }
        let summary = read_capture(file).map_err(|error| {
            ProviderError::new(reason!("host.traffic_unreadable", detail = error))
        })?;
        let (outside, local): (Vec<_>, Vec<_>) = summary
            .conversations
            .iter()
            .partition(|each| is_outside(&each.destination));
        let outside: Vec<Value> = outside
            .iter()
            .map(|each| {
                json!({
                    "protocol": each.protocol, "address": each.destination,
                    "port": each.port, "packets": each.packets, "bytes": each.bytes,
                })
            })
            .collect();
        Ok(data!({
            "recording": true, "file": path, "size": size, "packets": summary.packets,
            "outside": outside,
            "local": local.iter().map(|each| each.packets).sum::<u64>(),
            "names": summary.dns_queries,
        }))
    }

    /// Reach a service elsewhere from inside this guest, as a user of it
    /// would (`probe.rs`), and say what came back.
    fn probe(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let ask = probe::Ask::from(&action.arguments).map_err(ProviderError::new)?;
        let windows = self.guest_family("network.probe")? == "windows";
        let (command, words) = ask.argv(windows, POWERSHELL);
        if ask.kind == probe::Kind::Postgresql {
            let arguments = words.iter().map(|word| json!(word)).collect::<Vec<_>>();
            // Credentials are transient guest arguments, never process.created events.
            let result = self.with_agent("network.probe", |agent| {
                agent.run(&command, &arguments, Some(Duration::from_secs(15)))
            })?;
            return Ok(ask.read(
                result
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ));
        }
        let run = Action::new(
            "process.execute",
            data!({"command": command, "arguments": words, "timeout": 15}),
        )
        .expect("a valid capability");
        let result = self.execute(&run)?;
        let stdout = result
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(ask.read(stdout))
    }

    fn interfaces(&mut self) -> Result<Data, ProviderError> {
        let interfaces = self.with_agent("network.interfaces", |agent| agent.interfaces())?;
        let described: Vec<Value> = interfaces.iter().map(interface).collect();
        Ok(data!({"interfaces": described}))
    }

    /// A folder of this computer's, plugged in as a read-only USB drive --
    /// for putting the guest tools in front of somebody whose guest has no
    /// agent yet, the one thing the agent itself can never do.
    fn insert_media(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let folder = Self::required(action, "folder")?;
        if !Path::new(&folder).is_dir() {
            return Err(ProviderError::new(reason!(
                "engine.no_such_file",
                path = folder
            )));
        }
        let label = match action.argument("label") {
            Some(value) if truthy(value) => py_str(value),
            _ => "BEATBOX".to_owned(),
        };
        let fresh = self
            .require_monitor()?
            .insert_media(&folder, &label)
            .map_err(|error| ProviderError::new(error.reason))?;
        self.media = Some((folder.clone(), label.clone()));
        if fresh {
            self.emit("media.inserted", data!({"folder": folder, "label": label}));
        }
        Ok(data!({"folder": folder, "label": label, "already": !fresh}))
    }

    /// A folder of the person's, plugged in as a pen: read-only, in a slot of
    /// its own, and refused before QEMU would refuse it when it holds more
    /// than a pen can. One at a time: a second replaces the first.
    fn plug_pen(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let folder = Self::required(action, "folder")?;
        if !Path::new(&folder).is_dir() {
            return Err(ProviderError::new(reason!(
                "engine.no_such_file",
                path = folder
            )));
        }
        let size = folder_size(Path::new(&folder), PEN_LIMIT_BYTES);
        if size > PEN_LIMIT_BYTES {
            return Err(ProviderError::new(reason!(
                "host.pen_too_big",
                limit = PEN_LIMIT_BYTES / 1024 / 1024,
            )));
        }
        if self.pen.is_some() {
            self.eject_pen()?;
        }
        self.require_monitor()?
            .insert_pen(&folder, PEN_LABEL)
            .map_err(|error| ProviderError::new(error.reason))?;
        self.pen = Some(folder.clone());
        self.emit(
            "media.inserted",
            data!({"folder": folder, "label": PEN_LABEL}),
        );
        Ok(data!({"folder": folder, "label": PEN_LABEL, "bytes": size}))
    }

    fn eject_pen(&mut self) -> Result<Data, ProviderError> {
        self.require_monitor()?
            .eject_pen()
            .map_err(|error| ProviderError::new(error.reason))?;
        if let Some(folder) = self.pen.take() {
            self.emit("media.ejected", data!({"folder": folder}));
        }
        Ok(data!({}))
    }

    fn snapshot_create(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let name = Self::required(action, "name")?;
        if name == SAVED_STATE {
            return Err(ProviderError::new(reason!(
                "machine.bad_snapshot_name",
                name = name
            )));
        }
        self.require_monitor()?
            .snapshot_save(&name)
            .map_err(|error| ProviderError::new(error.reason))?;
        if !self.snapshots.contains(&name) {
            self.snapshots.push(name.clone());
        }
        if let Some(setup) = self.setup_now() {
            self.setups.insert(name.clone(), setup);
            self.keep_setups();
        }
        self.emit("snapshot.created", data!({"name": name}));
        Ok(data!({"name": name}))
    }

    fn snapshot_restore(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let name = Self::required(action, "name")?;
        if !self.snapshots.contains(&name) {
            return Err(ProviderError::new(reason!(
                "engine.no_such_snapshot",
                name = name
            )));
        }
        // QEMU loads a save point only onto the devices it was taken on, and
        // finds out half way: after it has put the disk back. So a save point
        // taken on other hardware -- another network, most often -- is
        // refused here, before anything is touched.
        if let (Some(was), Some(now)) = (self.setups.get(&name), self.setup_now())
            && let Some((setting, was, now)) = differs(was, &now)
        {
            return Err(ProviderError::new(reason!(
                "machine.snapshot_setup_differs",
                name = name,
                setting = setting,
                was = was,
                now = now,
            )));
        }
        match self.require_monitor()?.snapshot_load(&name) {
            Ok(()) => {
                self.emit("snapshot.restored", data!({"name": name}));
                Ok(data!({"name": name}))
            }
            Err(error) => Err(self.switch_off_after_failed_restore(&name, error)),
        }
    }

    /// VirtualBox's "Save the machine state": memory and disk kept in the
    /// overlay, then QEMU gone without the guest being asked to shut down.
    /// The next start carries on from there.
    fn save_state(&mut self) -> Result<Data, ProviderError> {
        self.keep_screen();
        self.require_monitor()?
            .snapshot_save(SAVED_STATE)
            .map_err(|error| ProviderError::new(error.reason))?;
        self.saved_state = true;
        if let Some(mut setup) = self.setup_now() {
            // What was plugged in while it ran: back in before `loadvm`,
            // which refuses a USB drive it does not find.
            setup["pen_folder"] = json!(self.pen);
            setup["media_plugged"] = json!(self.media);
            self.setups.insert(SAVED_STATE.into(), setup);
            self.keep_setups();
        }
        if let Some(monitor) = self.monitor.as_mut() {
            let _ = monitor.quit();
        }
        match self.machine.as_mut() {
            Some(machine) => {
                if machine.wait(STOP_TIMEOUT).is_none() {
                    machine.stop(STOP_TIMEOUT);
                }
            }
            None => {
                self.forget_channels();
                self.forget_sockets();
            }
        }
        self.halted_because = Some(reason!("host.state_saved"));
        self.emit("snapshot.state_saved", Data::new());
        Ok(Data::new())
    }

    /// The guest's screen kept beside the overlay, so the page can show how
    /// a machine looked when it went off. Nothing for a machine without a
    /// screen or not running, and a failure costs only the picture.
    fn keep_screen(&mut self) {
        if !flag(&self.config, "display", false) {
            return;
        }
        let path = self.workspace.join(SCREEN);
        if let Ok(monitor) = self.require_monitor() {
            let _ = monitor.screendump(&path);
        }
    }

    /// `display.picture`: the screen now while it runs, else as it last
    /// was, and when that was in seconds since 1970. Never taken: empty and 0.
    fn picture(&mut self) -> Data {
        self.keep_screen();
        let path = self.workspace.join(SCREEN);
        let taken = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_secs());
        let picture = fs::read(&path)
            .map(|bytes| base64::encode(&bytes))
            .unwrap_or_default();
        data!({"picture": picture, "taken": taken})
    }

    /// Back to where [`Self::save_state`] left it, then forgotten: a
    /// second start would otherwise go back to the same moment again.
    ///
    /// QEMU loads a state only onto the hardware it was saved on. Changed
    /// since (memory, network), it is dropped and the machine boots afresh.
    fn resume_saved_state(&mut self) -> Result<(), ProviderError> {
        self.plug_back();
        // ponytail: hardware changed while saved drops the state; refusing
        // the change while one is saved would keep it instead.
        let changed = match (self.setups.get(SAVED_STATE), self.setup_now()) {
            (Some(was), Some(now)) => differs(was, &now).map(|(setting, was, now)| {
                reason!(
                    "machine.snapshot_setup_differs",
                    name = SAVED_STATE,
                    setting = setting,
                    was = was,
                    now = now,
                )
            }),
            _ => None,
        };
        let loaded = match changed {
            None => self.require_monitor()?.snapshot_load(SAVED_STATE),
            Some(why) => {
                log::warn!(
                    "BeatBox: {} changed since its state was saved; booting afresh",
                    self.identity.id
                );
                self.state_lost = Some(why);
                Ok(())
            }
        };
        // Forgotten whether or not it came back: one that cannot be loaded
        // now never will be, and kept it would fail every start after.
        if let Err(error) = self.require_monitor()?.snapshot_delete(SAVED_STATE) {
            log::warn!(
                "BeatBox: {} could not forget its saved state: {:?}",
                self.identity.id,
                error.reason
            );
        }
        self.saved_state = false;
        self.setups.remove(SAVED_STATE);
        self.keep_setups();
        let Err(error) = loaded else {
            return Ok(());
        };
        // Half loaded and paused. The disk is the saved one either way, so
        // the reset button and a fresh boot of it is VirtualBox's "discard
        // the saved state": no memory of one moment runs on another's disk.
        log::warn!(
            "BeatBox: {} could not carry on from its saved state; booting afresh: {:?}",
            self.identity.id,
            error.reason
        );
        let monitor = self.require_monitor()?;
        match monitor.reset().and_then(|()| monitor.resume()) {
            Ok(()) => {
                self.state_lost = Some(error.reason);
                Ok(())
            }
            Err(_) => Err(self.switch_off_after_failed_restore(SAVED_STATE, error)),
        }
    }

    /// The USB drives the saved state was taken with, plugged in again. One
    /// whose folder has gone is left out, and the hardware check that
    /// follows drops the state.
    fn plug_back(&mut self) {
        let Some(setup) = self.setups.get(SAVED_STATE).cloned() else {
            return;
        };
        let Some(monitor) = self.monitor.as_mut() else {
            return;
        };
        if let Some([folder, label]) = setup["media_plugged"].as_array().map(Vec::as_slice)
            && let (Some(folder), Some(label)) = (folder.as_str(), label.as_str())
            && Path::new(folder).is_dir()
            && monitor.insert_media(folder, label).is_ok()
        {
            self.media = Some((folder.to_owned(), label.to_owned()));
        }
        if let Some(folder) = setup["pen_folder"].as_str()
            && Path::new(folder).is_dir()
            && monitor.insert_pen(folder, PEN_LABEL).is_ok()
        {
            self.pen = Some(folder.to_owned());
        }
    }

    /// `loadvm` put the disk back but not the memory. QEMU leaves the guest
    /// paused, one Resume away from running one moment's memory over
    /// another's disk, which ruins it. So switch it off and say why; started
    /// again, it boots from the save point's disk.
    fn switch_off_after_failed_restore(
        &mut self,
        name: &str,
        error: ControlError,
    ) -> ProviderError {
        let why = reason!("host.stopped_after_failed_restore", name = name);
        if let Some(monitor) = self.monitor.as_mut() {
            let _ = monitor.quit();
        }
        match self.machine.as_mut() {
            Some(machine) => {
                if machine.wait(STOP_TIMEOUT).is_none() {
                    machine.stop(STOP_TIMEOUT);
                }
                // Observed as a machine at rest, for this reason rather than
                // "shut down from inside".
                self.halted_because = Some(why);
                ProviderError::new(error.reason)
            }
            // A reattached guest: no process of ours to watch go, so it is
            // lost now, for the same reason.
            None => {
                self.forget_channels();
                self.forget_sockets();
                ProviderError::fatal(why)
            }
        }
    }

    /// The hardware a save point is taken on, as far as going back to it
    /// cares; `None` for a guest this process did not start.
    fn setup_now(&self) -> Option<Value> {
        let spec = &self.machine.as_ref()?.spec;
        Some(json!({
            "network": spec.network,
            "processors": spec.cpus,
            "memory": spec.memory_mb,
            "agent": !spec.agent_socket.is_empty(),
            "firmware": if spec.uefi_code.is_empty() { BIOS } else { UEFI },
            "pen": self.pen.is_some(),
            "media": self.media.is_some(),
        }))
    }

    /// Press the power button, and ask the agent too when there is one, and
    /// wait for QEMU to leave. Only for a running guest this process started:
    /// a paused one cannot shut itself down, and a reattached one has no
    /// process here to watch go.
    fn ask_to_shut_down(&mut self) {
        let grace = self.environment.shutdown_grace;
        let (Some(machine), Some(monitor)) = (self.machine.as_mut(), self.monitor.as_mut()) else {
            return;
        };
        if !machine.running() || !monitor.running().unwrap_or(false) {
            return;
        }
        if monitor.power_button().is_err() {
            return;
        }
        if let Some(agent) = self.agent.as_mut() {
            agent.shutdown();
        }
        machine.wait(grace);
    }

    /// Ask QEMU to go, then insist.
    ///
    /// The monitor first: `quit` lets QEMU close its disk cleanly. Then
    /// SIGTERM, and SIGKILL -- but only for a process this sandbox started. A
    /// reattached guest has no process of ours, and `quit` is the whole of
    /// what this can do for one.
    fn switch_off(&mut self) -> Result<(), ProviderError> {
        if self.machine.is_none() && self.monitor.is_none() {
            self.forget_channels();
            // A guest found gone left its socket files behind; switched off,
            // it is a machine at rest, not one to look for again.
            if self.reattach_failed {
                self.forget_sockets();
                self.reattach_failed = false;
            }
            return Ok(());
        }
        if let Some(monitor) = self.monitor.as_mut()
            && monitor.quit().is_err()
        {
            log::debug!(
                "BeatBox: {} did not answer quit; terminating",
                self.identity.id
            );
        }
        self.forget_channels();
        self.pen = None;
        self.media = None;
        self.state_lost = None;
        self.guest_family = None;
        if let Some(mut machine) = self.machine.take() {
            machine.stop(STOP_TIMEOUT);
        }
        // QEMU unlinks its own QMP socket on a clean quit; this covers the
        // cases where it did not, so the next observation finds no file and
        // reads "not there" rather than attempting, and failing, a reattach.
        self.forget_sockets();
        Ok(())
    }

    fn keep_setups(&self) {
        let path = self.workspace.join(SAVE_POINTS);
        if self.setups.is_empty() {
            let _ = fs::remove_file(path);
            return;
        }
        let text =
            serde_json::to_string_pretty(&Value::Object(self.setups.clone())).unwrap_or_default();
        if let Err(error) = fs::write(&path, text) {
            log::warn!("BeatBox: could not write {}: {error}", path.display());
        }
    }

    fn snapshot_delete(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let name = Self::required(action, "name")?;
        if !self.snapshots.contains(&name) {
            return Err(ProviderError::new(reason!(
                "engine.no_such_snapshot",
                name = name
            )));
        }
        self.require_monitor()?
            .snapshot_delete(&name)
            .map_err(|error| ProviderError::new(error.reason))?;
        self.snapshots.retain(|kept| kept != &name);
        if self.setups.remove(&name).is_some() {
            self.keep_setups();
        }
        self.emit("snapshot.deleted", data!({"name": name}));
        Ok(data!({"name": name}))
    }
}

impl SandboxProvider for HostSandbox {
    /// Narrowed to what *this* disk and *this* configuration can support.
    ///
    /// No agent asked for: nothing inside can be asked to do anything. A
    /// disc, or no disk of its own: nothing to snapshot into. Not a Linux
    /// guest: `process.list` runs `ps`. And `media.insert` needs a screen --
    /// the USB controller only exists on a machine with a window -- and is
    /// never offered to a hostile guest: nothing of this computer's is handed
    /// to one, read-only or not.
    fn capabilities(&self) -> Option<CapabilitySet> {
        let mut kept: Vec<&str> = CAPABILITIES.to_vec();
        if !self.wants_agent {
            kept.retain(|c| !NEEDS_AGENT.contains(c));
        }
        if self.optical {
            kept.retain(|c| !NEEDS_DISK.contains(c));
        }
        // Undescribed is asked of the guest when it runs; described as
        // something with neither `ps` nor `tasklist` is not.
        if !matches!(self.family().as_str(), "linux" | "windows") {
            kept.retain(|c| !matches!(*c, "process.list" | "process.kill"));
        }
        if !flag(&self.config, "display", false) || flag(&self.config, "hostile", false) {
            kept.retain(|c| !matches!(*c, "media.insert" | "media.pen" | "media.eject"));
        }
        if !flag(&self.config, "display", false) {
            kept.retain(|c| *c != "display.picture");
        }
        Some(CapabilitySet::new(kept).expect("declared names are valid"))
    }

    /// Make the disk and the room this guest runs in. Nothing boots yet.
    fn create(&mut self) -> Result<(), ProviderError> {
        if self.disk.is_empty() {
            return Err(ProviderError::new(reason!("host.no_disk")));
        }
        if !Path::new(&self.disk).is_file() {
            return Err(ProviderError::new(reason!(
                "machine.no_such_image",
                path = self.disk
            )));
        }
        // An .ova is a tar archive, which QEMU would read as a raw disk and
        // boot nothing from: its disk has to be taken out first.
        if self.disk.to_lowercase().ends_with(".ova") {
            return Err(ProviderError::new(reason!("host.ova_is_an_archive")));
        }
        let _ = fs::create_dir_all(&self.workspace);
        if !self.optical {
            let qemu = (self.environment.qemu)();
            create_overlay(
                &self.disk,
                &self.overlay.to_string_lossy(),
                &qemu,
                self.environment.runner.as_ref(),
            )
            .map_err(first)?;
        }
        self.spec_of(false)?; // validate now, rather than at the first start
        Ok(())
    }

    fn start(&mut self) -> Result<(), ProviderError> {
        if self.machine.as_mut().is_some_and(RunningMachine::running) {
            return Ok(());
        }
        if !Path::new(&self.disk).is_file() {
            return Err(ProviderError::new(reason!(
                "machine.no_such_image",
                path = self.disk
            )));
        }
        let spec = self.spec_of(false)?;
        if let Some(free) = (self.environment.available_memory_mb)() {
            let free = free as i64;
            // Asked before, rather than learnt after: a guest given more than
            // the host has is not refused by QEMU, it boots, grows into its
            // memory, and is killed by the kernel minutes later.
            if spec.memory_mb + QEMU_OVERHEAD_MB > free {
                return Err(ProviderError::new(reason!(
                    "host.not_enough_memory",
                    wanted = gigabytes(spec.memory_mb),
                    free = gigabytes((free - QEMU_OVERHEAD_MB).max(0)),
                )));
            }
        }
        // The channels are only given somewhere to listen now, at the moment
        // of starting: on Windows that means picking free ports.
        let uuid = (self.environment.uuid)();
        self.prepare_display_password(&uuid)?;
        let prepared = self.spec_of(true)?;
        self.pen = None;
        self.media = None;
        self.guest_family = None;
        let qemu = (self.environment.qemu)();
        self.machine = Some(start_machine(&prepared, &qemu, &uuid).map_err(first)?);
        match Monitor::connect(&self.monitor_socket, MONITOR_TIMEOUT) {
            Ok(monitor) => self.monitor = Some(monitor),
            Err(error) => {
                // QEMU started and will not answer. Take it away rather than
                // leave a guest running that nothing can stop.
                self.kill();
                return Err(ProviderError {
                    reason: Some(error.reason),
                    fatal: true,
                });
            }
        }
        if self.saved_state {
            self.resume_saved_state()?;
        }
        self.connect_agent(AGENT_TIMEOUT);
        Ok(())
    }

    /// Ask the guest to shut down, then switch it off.
    ///
    /// Switching off alone is a power cut, and Windows pays for those (a
    /// broken profile, a shell that won't come up). So press the power
    /// button first and give it [`Environment::shutdown_grace`].
    fn stop(&mut self) -> Result<(), ProviderError> {
        self.keep_screen();
        self.ask_to_shut_down();
        self.switch_off()
    }

    /// Take the guest away and the disk with it. Survives being called on a
    /// sandbox that never started, and on one that failed half way through
    /// being created.
    fn destroy(&mut self) -> Result<(), ProviderError> {
        // Its disk goes with it: nothing in there is worth a clean shutdown.
        if self.switch_off().is_err() {
            self.kill();
        }
        let _ = fs::remove_dir_all(&self.workspace);
        self.snapshots.clear();
        Ok(())
    }

    /// What is true about this guest, asked of QEMU and then of the guest.
    ///
    /// `alive` is the QEMU process where there is one, and the monitor where
    /// there is not -- which happens for exactly one reason: this sandbox was
    /// restored for a guest some earlier process started. A guest that has
    /// merely stopped answering its agent is a sandbox with less to say, not
    /// one that is gone.
    fn observe(&mut self) -> Observation {
        if self.machine.is_none() && self.monitor.is_none() {
            if self.reattach_failed {
                // Asked before, and there was nobody there. Asking again would
                // cost the same connection attempt every single poll.
                return self.lost(reason!("host.guest_lost"));
            }
            if let Some(why) = self.halted_because.take() {
                // A reattached guest switched off from here: no process of
                // ours to see leave, so said once, now.
                return Observation {
                    alive: Some(false),
                    halted: true,
                    reason: Some(why),
                    facts: self.resting_facts(),
                    ..Observation::default()
                };
            }
            if !self.monitor_socket.exists() {
                // Never started -- or started, stopped, and both sockets
                // already cleaned up. It exists, and it holds a disk.
                return Observation {
                    alive: Some(true),
                    facts: self.resting_facts(),
                    ..Observation::default()
                };
            }
            if !self.reattach(RECONNECT_TIMEOUT) {
                // A file from a guest with nobody answering on it any more.
                self.reattach_failed = true;
                return self.lost(reason!("host.guest_lost"));
            }
        }
        if let Some(status) = self.machine.as_mut().and_then(RunningMachine::exit_status) {
            if status.code() == Some(0) {
                // QEMU leaves with 0 when the guest powers itself off:
                // somebody chose Shut down in there. A machine at rest.
                self.forget_channels();
                self.forget_sockets();
                self.machine = None;
                let why = self
                    .halted_because
                    .take()
                    .unwrap_or_else(|| reason!("host.guest_shut_down"));
                return Observation {
                    alive: Some(false),
                    halted: true,
                    reason: Some(why),
                    facts: self.resting_facts(),
                    ..Observation::default()
                };
            }
            // Nobody asks QEMU to go with SIGKILL. On Linux this is, nearly
            // always, the kernel's out-of-memory killer, and "exited with -9"
            // tells somebody whose computer ran out of memory nothing.
            let why = if killed(&status) {
                reason!("host.guest_killed")
            } else {
                reason!("host.guest_went", code = exit_code(&status))
            };
            // A power cut, not a broken machine: the disk is fine and it
            // starts again. `failed` would leave only reset, which wipes it.
            self.forget_channels();
            self.forget_sockets();
            self.machine = None;
            return Observation {
                alive: Some(false),
                halted: true,
                reason: Some(why),
                facts: self.resting_facts(),
                ..Observation::default()
            };
        }

        if self.wants_agent && self.agent.is_none() {
            let due = self
                .agent_asked
                .is_none_or(|asked| asked.elapsed() >= self.environment.agent_retry_every);
            if due {
                self.connect_agent(AGENT_PROBE_TIMEOUT);
            }
        }

        let mut facts = self.resting_facts();
        facts.extend(self.display_facts());
        // A reattached guest has no process of ours to read these from --
        // what was asked for is still known, and is the right answer: a guest
        // does not get more by outliving the process that started it.
        let mut used = self.config.resources.clone();
        if let Some(machine) = self.machine.as_ref() {
            facts.insert("accelerated".into(), json!(machine.accelerated));
            used = Resources::new([
                (CPU, machine.spec.cpus as f64),
                (MEMORY, machine.spec.memory_mb as f64),
            ])
            .unwrap_or_default();
        }
        if let Some(monitor) = self.monitor.as_mut() {
            match monitor.status() {
                Ok(status) => {
                    facts.insert("status".into(), json!(status));
                    let events = monitor.events(EVENT_POLL);
                    self.report(events);
                }
                Err(error) => {
                    facts.insert("status".into(), json!("unreachable"));
                    facts.insert("monitor_error".into(), json!(error.reason.to_string()));
                }
            }
        }
        if self.agent.is_some() {
            let guest = self.guest_facts();
            facts.extend(guest);
        }
        Observation {
            alive: Some(true),
            resources: used,
            facts,
            ..Observation::default()
        }
    }

    fn pause(&mut self) -> Result<(), ProviderError> {
        let monitor = self.require_monitor()?;
        match monitor.pause() {
            Ok(()) => Ok(()),
            Err(error) => Err(self.uncaught("pause", &error)),
        }
    }

    fn resume(&mut self) -> Result<(), ProviderError> {
        let monitor = self.require_monitor()?;
        match monitor.resume() {
            Ok(()) => Ok(()),
            Err(error) => Err(self.uncaught("resume", &error)),
        }
    }

    /// Back to the disk it was created with, as if nothing had run: stop,
    /// throw the overlay away, make a new one. Not a snapshot restore, and
    /// every snapshot taken inside the old overlay goes with it, because they
    /// lived in the file that was deleted.
    fn reset(&mut self) -> Result<(), ProviderError> {
        if !Path::new(&self.disk).is_file() {
            return Err(ProviderError::new(reason!(
                "machine.no_such_image",
                path = self.disk
            )));
        }
        // The overlay is thrown away next: a clean shutdown would be wasted.
        self.switch_off()?;
        if self.optical {
            return Ok(()); // nothing was ever written; stopping is the whole of it
        }
        let _ = fs::remove_file(&self.overlay);
        // The variables came with the disk they were written by: a fresh disk
        // gets fresh ones, made again at the next start.
        let _ = fs::remove_file(self.workspace.join(EFI_VARS));
        self.snapshots.clear();
        self.saved_state = false;
        self.setups.clear();
        self.keep_setups();
        let qemu = (self.environment.qemu)();
        create_overlay(
            &self.disk,
            &self.overlay.to_string_lossy(),
            &qemu,
            self.environment.runner.as_ref(),
        )
        .map_err(|error| ProviderError {
            reason: error.first().cloned(),
            fatal: true,
        })
    }

    /// Processors, memory, network, the agent channel, the screen -- read
    /// again at the next start. Validated here, so a configuration QEMU would
    /// refuse is refused now rather than at the next start.
    fn reconfigure(&mut self, config: &SandboxConfig) -> Result<(), ProviderError> {
        for key in FIXED {
            let asked = config.setting(key).cloned().unwrap_or_else(|| json!(""));
            let had = self
                .config
                .setting(key)
                .cloned()
                .unwrap_or_else(|| json!(""));
            if asked != had {
                return Err(ProviderError::new(reason!(
                    "host.fixed_setting",
                    setting = key
                )));
            }
        }
        let previous = std::mem::replace(&mut self.config, config.clone());
        if let Err(error) = self.spec_of(false) {
            self.config = previous;
            return Err(error);
        }
        self.wants_agent = flag(config, "agent", true);
        Ok(())
    }

    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        match action.capability() {
            "process.execute" => self.execute(action),
            "process.shell" => self.shell(action),
            "process.list" => self.processes(),
            "process.kill" => self.kill_process(action),
            "filesystem.read" => self.read(action),
            "filesystem.write" => self.write(action),
            "network.interfaces" => self.interfaces(),
            "network.probe" => self.probe(action),
            "network.traffic" => self.traffic(),
            "snapshot.create" => self.snapshot_create(action),
            "snapshot.restore" => self.snapshot_restore(action),
            "snapshot.delete" => self.snapshot_delete(action),
            "snapshot.list" => Ok(data!({"snapshots": self.snapshots})),
            "snapshot.save_state" => self.save_state(),
            "media.insert" => self.insert_media(action),
            "media.pen" => self.plug_pen(action),
            "media.eject" => self.eject_pen(),
            "display.picture" => Ok(self.picture()),
            other => Err(unsupported(other)),
        }
    }
}

// No Drop that stops the guest, on purpose: guests outlive the daemon and
// the next one reattaches them. Rust's Child doesn't kill on drop either.

fn first(error: MachineError) -> ProviderError {
    ProviderError {
        reason: error.first().cloned(),
        fatal: false,
    }
}

/// `4096` -> `4`, `3000` -> `2.9`: GiB with one decimal, trailing zeros off.
pub fn gigabytes(mebibytes: i64) -> String {
    let text = format!("{:.1}", mebibytes as f64 / 1024.0);
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(unix)]
fn killed(status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    status.signal() == Some(9)
}

#[cfg(not(unix))]
fn killed(_status: &std::process::ExitStatus) -> bool {
    false
}

/// An exit code, negative for a signal (`-9`).
fn exit_code(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("-{signal}");
        }
    }
    status.code().map_or_else(|| "-1".into(), |c| c.to_string())
}

/// `arguments` with every `secrets` value in them as `••••`.
fn concealed(arguments: &[Value], secrets: &[&str]) -> Vec<Value> {
    arguments
        .iter()
        .map(|argument| match argument.as_str() {
            Some(text) => Value::from(
                secrets
                    .iter()
                    .fold(text.to_owned(), |text, secret| text.replace(secret, "••••")),
            ),
            None => argument.clone(),
        })
        .collect()
}

fn addresses_of(entry: &Value) -> Vec<Value> {
    entry
        .get("ip-addresses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// How many bytes a folder holds, counting no further than just past
/// `limit`: a pen only needs to know whether it fits. Links are not
/// followed, as `vvfat` does not follow them either.
pub fn folder_size(folder: &Path, limit: u64) -> u64 {
    let mut total = 0;
    let mut waiting = vec![folder.to_path_buf()];
    while let Some(next) = waiting.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                waiting.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
                if total > limit {
                    return total;
                }
            }
        }
    }
    total
}

/// `tasklist /v /fo csv /nh`, as the same shape `parse_ps` makes: image
/// name as the command, the user, and the status as the state. Windows does
/// not say whose child a process is here, so `ppid` is 0.
pub fn parse_tasklist(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| {
            let fields = csv_fields(line.trim());
            let command = fields.first()?.clone();
            let pid = fields.get(1)?.parse::<i64>().ok()?;
            if command.is_empty() {
                return None;
            }
            Some(json!({
                "pid": pid,
                "ppid": 0,
                "user": fields.get(6).cloned().unwrap_or_default(),
                "state": fields.get(5).cloned().unwrap_or_default(),
                "command": command,
            }))
        })
        .collect()
}

/// One CSV line of `"quoted","fields"`, as tasklist writes them: a doubled
/// quote inside one is a quote.
fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            other => field.push(other),
        }
    }
    fields.push(field);
    fields
}

/// `ps` output into records. Five columns, split from the left, `comm` last
/// because a command can contain spaces and nothing else can.
pub fn parse_ps(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?;
            let ppid = parts.next()?;
            let user = parts.next()?;
            let state = parts.next()?;
            // Whatever follows the fourth field, spaces and all.
            let mut rest = line.trim_start();
            for _ in 0..4 {
                rest = rest.trim_start();
                rest = &rest[rest.find(char::is_whitespace)?..];
            }
            let command = rest.trim();
            if command.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let ppid = if ppid.chars().all(|c| c.is_ascii_digit()) {
                ppid.parse::<i64>().unwrap_or(0)
            } else {
                0
            };
            Some(json!({
                "pid": pid.parse::<i64>().ok()?,
                "ppid": ppid,
                "user": user,
                "state": state,
                "command": command,
            }))
        })
        .collect()
}

/// The cable in what a network sandbox's `network.attach` answered, which is
/// all of that answer a host reads: where the switch is, which end is this
/// card's, and the MAC the switch will accept from it. The MAC goes into a
/// QEMU option, so it has to be a MAC and nothing more.
fn cable(attachment: &Map<String, Value>) -> Result<Cable, ProviderError> {
    let field = |key: &str| {
        attachment
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let (switch, port, mac) = (field("switch"), field("port"), field("mac"));
    let is_mac = mac.len() == 17
        && mac
            .split(':')
            .all(|pair| pair.len() == 2 && pair.chars().all(|c| c.is_ascii_hexdigit()));
    if switch.is_empty() || port.is_empty() || !is_mac {
        return Err(ProviderError::new(reason!(
            "machine.bad_network",
            mode = NET_ATTACHED
        )));
    }
    Ok(Cable { switch, port, mac })
}

/// One interface, as the guest sees it -- which is the only way it can be seen.
pub fn interface(entry: &Value) -> Value {
    let addresses: Vec<Value> = addresses_of(entry)
        .iter()
        .map(|address| {
            json!({
                "address": address.get("ip-address").cloned().unwrap_or(json!("")),
                "prefix": address.get("prefix").cloned().unwrap_or(json!(0)),
                "family": address.get("ip-address-type").cloned().unwrap_or(json!("")),
            })
        })
        .collect();
    json!({
        "name": entry.get("name").cloned().unwrap_or(json!("")),
        "mac": entry.get("hardware-address").cloned().unwrap_or(json!("")),
        "addresses": addresses,
    })
}

/// The save points' hardware, as written beside the overlay, for the save
/// points still in it.
fn read_setups(path: &Path, names: &[String]) -> Map<String, Value> {
    let Ok(text) = fs::read_to_string(path) else {
        return Map::new();
    };
    let mut setups = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(setups)) => setups,
        _ => Map::new(),
    };
    setups.retain(|name, _| names.contains(name));
    setups
}

#[cfg(unix)]
fn private_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn private_file(_path: &Path) {}

/// The first thing about the hardware that is not as it was: which setting,
/// what it was, what it is. Isolated and NAT are the same card to QEMU, so
/// going between them is not a difference; a record missing a setting (an
/// older one) does not count it either.
fn differs(was: &Value, now: &Value) -> Option<(&'static str, String, String)> {
    let card = |value: &Value| match value.as_str() {
        Some("isolated" | "nat") => "user".to_owned(),
        _ => said(value),
    };
    for setting in [
        "network",
        "processors",
        "memory",
        "agent",
        "firmware",
        "pen",
        "media",
    ] {
        let (before, after) = (&was[setting], &now[setting]);
        if before.is_null() {
            continue;
        }
        let same = if setting == "network" {
            card(before) == card(after)
        } else {
            before == after
        };
        if !same {
            return Some((setting, said(before), said(after)));
        }
    }
    None
}

fn said(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::concealed;
    use serde_json::json;

    #[test]
    fn a_concealed_value_is_never_said_wherever_it_sits() {
        let arguments = [
            json!("serve"),
            json!("--password=hunter2"),
            json!("hunter2"),
            json!(5),
        ];
        assert_eq!(
            concealed(&arguments, &["hunter2"]),
            [
                json!("serve"),
                json!("--password=••••"),
                json!("••••"),
                json!(5)
            ]
        );
        assert_eq!(concealed(&arguments, &[]), arguments);
    }
}
