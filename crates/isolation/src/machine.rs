//! A guest with its own kernel: what to run, and the exact argv to run it with.
//!
//! A machine's safety is almost all one list, the argv, so one function
//! builds it, held flag for flag to `golden/qemu/machine.json`.
//!
//! **Four refusals to be convenient:** disposable by default (`-snapshot`),
//! no network unless asked (`-nic none`), nothing shared (no 9p, virtiofs,
//! clipboard or passthrough), and the host's accelerator with none of its
//! identity (KVM, WHPX on Windows, else TCG, said out loud).
//!
//! The host's QEMU is a [`Qemu`] value, probed once and handed in, so any
//! host's argv can be built on any host.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use beatbox_reasons::{Reason, reason};
use serde_json::{Value, json};

use crate::endpoint;
use crate::host_os::{WINDOWS, qemu_program, whpx_available};
use crate::instrument::{FRIDA_PORT, instrumentation_forward};
use crate::runner::Runner;

/// QEMU's system emulator for this architecture.
pub const QEMU_BINARY: &str = "qemu-system-x86_64";
/// QEMU's disk tool.
pub const QEMU_IMG: &str = "qemu-img";
/// What every disk BeatBox makes is.
pub const DISK_FORMAT: &str = "qcow2";
/// Where KVM is, on Linux.
pub const KVM_DEVICE: &str = "/dev/kvm";

/// ISO 9660 stamps this at the start of its first volume descriptor, which
/// lives at sector 16 of a 2048-byte-sector image: 16 * 2048 + 1.
pub const ISO_MAGIC: &[u8; 5] = b"CD001";
/// See [`ISO_MAGIC`].
pub const ISO_MAGIC_OFFSET: u64 = 16 * 2048 + 1;

/// No card at all.
pub const NET_NONE: &str = "none";
/// A card, and SLIRP drops everything it sends outward.
pub const NET_ISOLATED: &str = "isolated";
/// A card with a way out.
pub const NET_NAT: &str = "nat";
/// How a guest is attached to the world, in the order of danger.
pub const NETWORK_MODES: [&str; 3] = [NET_NONE, NET_ISOLATED, NET_NAT];
/// A card cabled to a network sandbox's switch ([`Cable`]). Not one of
/// [`NETWORK_MODES`]: nobody picks it from a list, a network hands it out.
pub const NET_ATTACHED: &str = "attached";

/// A card's cable to a switch: every frame, as one datagram, from `port` to
/// `switch` and back. All a machine knows of a network sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cable {
    /// Where the switch listens.
    pub switch: String,
    /// This card's end, which QEMU binds.
    pub port: String,
    /// The card's Ethernet address. The switch refuses any other.
    pub mac: String,
}

/// The display backends that put a window on somebody's screen. Only these
/// count: Debian's base QEMU lists `egl-headless`, `curses`, `dbus` and
/// `spice-app`, none of which QEMU picks on its own.
pub const WINDOWED: [&str; 3] = ["gtk", "sdl", "cocoa"];

/// Hyper-V enlightenments: paravirtual clock and timers, no spinning on
/// locks or the APIC. Measured on Windows 10: cheaper boot (25-80% of a core
/// against 31-94%), same idle (8%). Don't claim more for them.
pub const HYPERV_ENLIGHTENMENTS: [&str; 9] = [
    "hv-relaxed=on",
    "hv-vapic=on",
    "hv-spinlocks=0x1fff",
    "hv-vpindex=on",
    "hv-runtime=on",
    "hv-time=on",
    "hv-synic=on",
    "hv-stimer=on",
    "hv-frequencies=on",
];

/// Raised when a machine cannot be prepared or started: every reason at
/// once, named rather than written out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineError {
    /// Everything wrong, in the order it was found.
    pub reasons: Vec<Reason>,
}

impl MachineError {
    /// One reason.
    pub fn new(reason: Reason) -> Self {
        Self {
            reasons: vec![reason],
        }
    }

    /// The first reason, which is the one a single refusal carries.
    pub fn first(&self) -> Option<&Reason> {
        self.reasons.first()
    }
}

impl std::fmt::Display for MachineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = self.reasons.iter().map(ToString::to_string).collect();
        formatter.write_str(&names.join(" "))
    }
}

impl std::error::Error for MachineError {}

/// Whether this image is a disc rather than a disk.
///
/// A disk boots by its MBR over AHCI; a disc by El Torito with `-cdrom`. A
/// Windows ISO has no MBR, so on the disk path nothing boots while the guest
/// looks running. Sniffed, not trusted from the name; the extension is only
/// the fallback for a file that can't be opened.
pub fn is_optical(image: &str) -> bool {
    if image.is_empty() {
        return false;
    }
    let sniffed = (|| -> io::Result<bool> {
        let mut file = File::open(image)?;
        file.seek(SeekFrom::Start(ISO_MAGIC_OFFSET))?;
        let mut magic = [0u8; 5];
        let mut read = 0;
        while read < magic.len() {
            let n = file.read(&mut magic[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        Ok(read == magic.len() && &magic == ISO_MAGIC)
    })();
    sniffed.unwrap_or_else(|_| image.to_lowercase().ends_with(".iso"))
}

/// A Windows disc's language as Setup names it (`pt-PT`), from Microsoft's
/// volume label `CCCOMA_X64FRE_PT-PT_DV9`. An unattended install must name
/// it, and a wrong one stops at a question nobody sees. None otherwise,
/// including the Media Creation Tool's `ESD-ISO`, servers (`X64FREE`, other
/// keys) and anything not x64.
///
/// ponytail: the label is a convention, not a format. Reading
/// `sources/lang.ini` needs a UDF reader, worth it when a disc with a
/// label of its own has to install unwatched.
pub fn windows_installer_language(image: &str) -> Option<String> {
    let mut label = [0u8; 32];
    let mut file = File::open(image).ok()?;
    file.seek(SeekFrom::Start(ISO_MAGIC_OFFSET - 1 + 40)).ok()?;
    file.read_exact(&mut label).ok()?;
    if !is_optical(image) {
        return None;
    }
    let label = String::from_utf8_lossy(&label).trim().to_owned();
    let mut parts = label.split('_');
    // x64 only: the guests BeatBox runs, and the architecture the answers name.
    parts.find(|part| *part == "X64FRE")?;
    let language = parts.next()?;
    let words: Vec<&str> = language.split('-').collect();
    let well_formed = words.len() >= 2
        && words.iter().all(|word| {
            (2..=4).contains(&word.len()) && word.chars().all(|c| c.is_ascii_alphabetic())
        });
    if !well_formed {
        return None;
    }
    // Setup's own spelling: language lower, script title, region upper.
    let last = words.len() - 1;
    let spelled: Vec<String> = words
        .iter()
        .enumerate()
        .map(|(at, word)| match at {
            0 => word.to_ascii_lowercase(),
            _ if at == last => word.to_ascii_uppercase(),
            _ => word[..1].to_ascii_uppercase() + &word[1..].to_ascii_lowercase(),
        })
        .collect();
    Some(spelled.join("-"))
}

/// Whether `/dev/kvm` exists and this user can open it. Saying "KVM" while
/// QEMU falls back to emulation makes a 10x slower run look like hardware.
pub fn kvm_available() -> bool {
    Path::new(KVM_DEVICE).exists()
        && OpenOptions::new()
            .read(true)
            .write(true)
            .open(KVM_DEVICE)
            .is_ok()
}

/// What of QEMU this host has, read once and handed around, so that an argv
/// can be built for any host on any host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qemu {
    /// The system emulator: a full path where one was found, the bare name
    /// otherwise, so a failure to start names what was looked for.
    pub system: String,
    /// `qemu-img`, the same way.
    pub image: String,
    /// Whether the system emulator was found at all.
    pub system_found: bool,
    /// Whether `qemu-img` was found at all.
    pub image_found: bool,
    /// KVM, usable by this user.
    pub kvm: bool,
    /// Windows' own hypervisor.
    pub whpx: bool,
    /// `BEATBOX_QEMU_CPU`: a CPU model chosen by hand, away from KVM.
    pub cpu: Option<String>,
}

impl Qemu {
    /// What this host has, now.
    pub fn here() -> Self {
        let system = qemu_program(QEMU_BINARY);
        let image = qemu_program(QEMU_IMG);
        Self {
            system_found: system.is_some(),
            image_found: image.is_some(),
            system: path_or(system, QEMU_BINARY),
            image: path_or(image, QEMU_IMG),
            kvm: kvm_available(),
            whpx: whpx_available(),
            cpu: std::env::var("BEATBOX_QEMU_CPU")
                .ok()
                .filter(|c| !c.is_empty()),
        }
    }

    /// Whether a guest here runs on a hypervisor: KVM, or Windows' WHPX.
    pub fn accelerated(&self) -> bool {
        self.kvm || self.whpx
    }
}

fn path_or(found: Option<PathBuf>, name: &str) -> String {
    found.map_or_else(|| name.to_owned(), |p| p.to_string_lossy().into_owned())
}

/// What to run, and what it is allowed to touch.
///
/// `image` is supplied by the user and never fetched: a sandbox whose
/// contents arrived from somewhere the user did not choose is not a sandbox.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineSpec {
    /// What QEMU calls it.
    pub name: String,
    /// The disk or disc it boots.
    pub image: String,
    /// `-m`.
    pub memory_mb: i64,
    /// `-smp`.
    pub cpus: i64,
    /// `none`, `isolated`, `nat`, or [`NET_ATTACHED`] with a [`Cable`].
    pub network: String,
    /// The cable, when `network` is [`NET_ATTACHED`].
    pub cable: Option<Cable>,
    /// No window.
    pub headless: bool,
    /// Where QEMU's own messages go. Empty: nowhere.
    pub log_path: String,
    /// Ports forwarded from the host into the guest, host:guest. Only
    /// meaningful with a network, and each one is a deliberate hole.
    pub forwarded_ports: Vec<(i64, i64)>,
    /// Set only by a caller that has said, in as many words, that what runs
    /// here is hostile. It relaxes nothing -- it forbids things.
    pub hostile: bool,
    /// Where to write every packet. Empty means no capture.
    pub capture_path: String,
    /// Open the one port the host needs to attach a debugger inside.
    pub instrument: bool,
    /// Whether every write dies with the process (`-snapshot`). `false`
    /// means *this disk is already the sandbox's own*: an overlay BeatBox
    /// made, backed by the user's image, deleted with the sandbox.
    pub disposable: bool,
    /// Where QEMU's control channel (QMP) listens. Empty means none.
    pub monitor_socket: String,
    /// Where a qemu-guest-agent inside is reached. Empty means none.
    pub agent_socket: String,
    /// Private VNC socket used as QEMU's primary display listener. Empty uses
    /// QEMU's native window (kept for direct library callers and installs).
    pub display_socket: String,
    /// Loopback endpoint for the WebSocket side of the VNC display.
    pub display_websocket: String,
    /// File containing the VNC password, never the process command line.
    pub display_password_file: String,
    /// UEFI's code (`firmware::Uefi`), mapped read-only. Empty means the PC
    /// BIOS, which is what every machine had before UEFI.
    pub uefi_code: String,
    /// This machine's own UEFI variables, as qcow2 so a save point can hold
    /// them too: QEMU refuses `savevm` beside a writable raw drive.
    pub uefi_vars: String,
}

impl Default for MachineSpec {
    fn default() -> Self {
        Self {
            name: "sandbox".into(),
            image: String::new(),
            memory_mb: 2048,
            cpus: 2,
            network: NET_NONE.into(),
            cable: None,
            headless: false,
            log_path: String::new(),
            forwarded_ports: Vec::new(),
            hostile: false,
            capture_path: String::new(),
            instrument: false,
            disposable: true,
            monitor_socket: String::new(),
            agent_socket: String::new(),
            display_socket: String::new(),
            display_websocket: String::new(),
            display_password_file: String::new(),
            uefi_code: String::new(),
            uefi_vars: String::new(),
        }
    }
}

impl MachineSpec {
    /// Everything wrong with this spec, in one pass, so a screen can show all
    /// of it at once rather than one thing per attempt.
    pub fn validate(&self) -> Vec<Reason> {
        let mut problems = Vec::new();
        if self.image.is_empty() {
            problems.push(reason!("machine.no_image"));
        } else if !Path::new(&self.image).is_file() {
            problems.push(reason!("machine.no_such_image", path = self.image));
        }
        let attached = self.network == NET_ATTACHED;
        if attached != self.cable.is_some()
            || !(attached || NETWORK_MODES.contains(&self.network.as_str()))
        {
            problems.push(reason!("machine.bad_network", mode = self.network));
        }
        // Forwarding and instrumenting are SLIRP's `hostfwd`, and a cabled
        // card has no SLIRP behind it.
        if attached && (!self.forwarded_ports.is_empty() || self.instrument) {
            problems.push(reason!("machine.ports_on_a_network"));
        }
        if self.memory_mb < 256 {
            problems.push(reason!("machine.too_little_memory"));
        }
        if self.cpus < 1 {
            problems.push(reason!("machine.too_few_cpus"));
        }
        // Here, not just in a dialog: a guard in one dialog is one call site
        // away from not existing.
        if self.hostile && self.network == NET_NAT {
            problems.push(reason!("machine.hostile_with_a_route_out"));
        }
        if !self.forwarded_ports.is_empty() && self.network == NET_NONE {
            problems.push(reason!("machine.ports_without_a_card"));
        }
        if self.instrument && self.network == NET_NONE {
            problems.push(reason!("machine.instrument_without_a_card"));
        }
        if !self.capture_path.is_empty() && self.network == NET_NONE {
            problems.push(reason!("machine.capture_without_a_card"));
        }
        if !self.uefi_code.is_empty() && !Path::new(&self.uefi_code).is_file() {
            problems.push(reason!("machine.no_uefi", path = self.uefi_code));
        }
        problems
    }

    /// As JSON for the goldens, with the UEFI pair only when there is one.
    pub fn to_value(&self) -> Value {
        let mut shape = json!({
            "name": self.name,
            "image": self.image,
            "memory_mb": self.memory_mb,
            "cpus": self.cpus,
            "network": self.network,
            "headless": self.headless,
            "log_path": self.log_path,
            "forwarded_ports": self.forwarded_ports.iter().map(|(h, g)| json!([h, g])).collect::<Vec<_>>(),
            "hostile": self.hostile,
            "capture_path": self.capture_path,
            "instrument": self.instrument,
            "disposable": self.disposable,
            "monitor_socket": self.monitor_socket,
            "agent_socket": self.agent_socket,
        });
        if let Some(cable) = &self.cable {
            shape["cable"] = json!({"switch": cable.switch, "port": cable.port, "mac": cable.mac});
        }
        if !self.uefi_code.is_empty() {
            shape["uefi_code"] = json!(self.uefi_code);
            shape["uefi_vars"] = json!(self.uefi_vars);
        }
        shape
    }
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

/// A network mode, as QEMU arguments.
///
/// `restrict=on` is what makes *isolated* mean something: the guest gets a
/// card and a DHCP lease, and SLIRP drops everything it sends towards the
/// host or the internet. e1000, not virtio-net-pci: Windows carries no virtio
/// driver, so the "cheaper" card was no card.
fn network_args(spec: &MachineSpec) -> Vec<String> {
    if spec.network == NET_NONE {
        return strings(&["-nic", "none"]);
    }
    if let Some(cable) = &spec.cable {
        return vec![
            "-netdev".into(),
            format!(
                "dgram,id=net0,local.type=unix,local.path={},remote.type=unix,remote.path={}",
                endpoint::option(&cable.port),
                endpoint::option(&cable.switch),
            ),
            "-device".into(),
            format!("e1000,netdev=net0,mac={}", cable.mac),
        ];
    }
    let mut options = vec!["user".to_owned(), "id=net0".to_owned()];
    if spec.network == NET_ISOLATED {
        options.push("restrict=on".into());
    }
    let mut forwards = spec.forwarded_ports.clone();
    if spec.instrument {
        let (host, guest) = instrumentation_forward(FRIDA_PORT);
        forwards.push((i64::from(host), i64::from(guest)));
    }
    for (host, guest) in forwards {
        options.push(format!("hostfwd=tcp::{host}-:{guest}"));
    }
    vec![
        "-netdev".into(),
        options.join(","),
        "-device".into(),
        "e1000,netdev=net0".into(),
    ]
}

/// How much of each packet a capture keeps. Every header and every DNS
/// question fit (at most 333 bytes, over IPv6), which is all the summary
/// reads; the rest of a payload is what fills a disk, and a capture runs for
/// as long as the machine does.
pub const CAPTURE_SNAPLEN: u32 = 512;

/// The guest's traffic, dumped where the host can read it. Attached to the
/// netdev, so it needs no cooperation and cannot be turned off from inside.
fn capture_args(spec: &MachineSpec) -> Vec<String> {
    if spec.capture_path.is_empty() || spec.network == NET_NONE {
        return Vec::new();
    }
    vec![
        "-object".into(),
        format!(
            "filter-dump,id=dump0,netdev=net0,maxlen={CAPTURE_SNAPLEN},file={}",
            endpoint::option(&spec.capture_path)
        ),
    ]
}

/// The two channels a host sandbox drives the guest through. The agent is a
/// virtio serial port, which is a channel and not a hole: it shares no
/// filesystem, and a disk without qemu-guest-agent simply never answers.
fn control_args(spec: &MachineSpec) -> Vec<String> {
    let mut args = Vec::new();
    if !spec.monitor_socket.is_empty() {
        args.push("-qmp".into());
        args.push(endpoint::qmp_argument(&spec.monitor_socket));
    }
    if !spec.agent_socket.is_empty() {
        args.extend(agent_args(&spec.agent_socket));
    }
    args
}

fn agent_args(socket: &str) -> Vec<String> {
    vec![
        "-chardev".into(),
        endpoint::chardev_argument(socket, "qga0"),
        "-device".into(),
        "virtio-serial".into(),
        "-device".into(),
        "virtserialport,chardev=qga0,name=org.qemu.guest_agent.0".into(),
    ]
}

/// A disk over AHCI, which every OS BeatBox might boot speaks out of the box
/// -- and the same call for an install and a run, because whatever bus
/// Windows was installed onto is the one bus it boots from.
///
/// `discard`/`detect-zeroes`: what the guest deletes or zeroes leaves the
/// file. `rotation_rate=1`: Windows trims instead of defragmenting, and a
/// defragmentation rewrites the whole disk into the overlay.
pub fn disk_args(path: &str, drive_id: &str) -> Vec<String> {
    vec![
        "-device".into(),
        "ahci,id=ahci0".into(),
        "-drive".into(),
        format!(
            "file={},if=none,id={drive_id},discard=unmap,detect-zeroes=unmap",
            endpoint::option(path)
        ),
        "-device".into(),
        format!("ide-hd,bus=ahci0.0,drive={drive_id},rotation_rate=1"),
    ]
}

/// UEFI, as two flash chips: the firmware, read-only, then this machine's
/// own variables. Nothing on a BIOS machine.
pub fn uefi_args(spec: &MachineSpec) -> Vec<String> {
    if spec.uefi_code.is_empty() {
        return Vec::new();
    }
    vec![
        "-drive".into(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            endpoint::option(&spec.uefi_code)
        ),
        "-drive".into(),
        format!(
            "if=pflash,format=qcow2,unit=1,file={}",
            endpoint::option(&spec.uefi_vars)
        ),
    ]
}

/// KVM with the host's own CPU, told it is being virtualised; WHPX on
/// Windows, with QEMU's own fallback to TCG; or TCG. `-cpu host` and the
/// hv-* list are KVM's -- given to WHPX or TCG, QEMU refuses to start.
pub fn accel_args(qemu: &Qemu) -> Vec<String> {
    if qemu.kvm {
        let cpu = std::iter::once("host")
            .chain(HYPERV_ENLIGHTENMENTS)
            .collect::<Vec<_>>()
            .join(",");
        return vec!["-accel".into(), "kvm".into(), "-cpu".into(), cpu];
    }
    let cpu: Vec<String> = qemu
        .cpu
        .as_ref()
        .map(|model| vec!["-cpu".to_owned(), model.clone()])
        .unwrap_or_default();
    let mut args = if qemu.whpx {
        strings(&["-accel", "whpx,kernel-irqchip=off", "-accel", "tcg"])
    } else {
        strings(&["-accel", "tcg"])
    };
    args.extend(cpu);
    args
}

/// A mouse that goes where the host's pointer is: a USB tablet reports
/// absolute positions, so there is no grab and no drift. On xHCI rather than
/// UHCI, because UHCI is polled for as long as the machine is up. Only for a
/// machine with a screen.
pub fn pointer_args() -> Vec<String> {
    strings(&[
        "-device",
        "qemu-xhci,id=usb0",
        "-device",
        "usb-tablet,bus=usb0.0",
    ])
}

/// Memory the guest is not using goes back to the host (free page
/// reporting), and a guest about to run out gives back first.
pub fn balloon_args() -> Vec<String> {
    strings(&[
        "-device",
        "virtio-balloon-pci,free-page-reporting=on,deflate-on-oom=on",
    ])
}

/// The exact argv the guest would be started with, on `qemu`, as `uuid`.
///
/// A fresh SMBIOS UUID every time: QEMU's default is all zeros everywhere,
/// and Windows Autopilot enrols by it, showing some real company's sign-in.
pub fn build_command(
    spec: &MachineSpec,
    qemu: &Qemu,
    uuid: &str,
) -> Result<Vec<String>, MachineError> {
    let problems = spec.validate();
    if !problems.is_empty() {
        return Err(MachineError { reasons: problems });
    }
    let mut command = vec![
        qemu.system.clone(),
        "-name".into(),
        spec.name.clone(),
        "-uuid".into(),
        uuid.to_owned(),
        "-m".into(),
        spec.memory_mb.to_string(),
        "-smp".into(),
        spec.cpus.to_string(),
    ];
    if spec.disposable {
        command.push("-snapshot".into());
    }
    if is_optical(&spec.image) {
        // A disc needs El Torito and q35: on `pc` a Windows 10 ISO's
        // cdboot.com dies silently. Disks keep `pc`, which they may have
        // been installed on.
        command.extend([
            "-machine".into(),
            "q35".into(),
            "-cdrom".into(),
            endpoint::option(&spec.image),
            "-boot".into(),
            "d".into(),
        ]);
    } else {
        command.extend(disk_args(&spec.image, "sysdisk"));
    }
    command.extend(uefi_args(spec));
    command.extend(accel_args(qemu));
    command.extend(balloon_args());
    command.extend(network_args(spec));
    command.extend(capture_args(spec));
    command.extend(control_args(spec));
    if spec.headless {
        command.extend(strings(&["-display", "none"]));
    } else if !spec.display_websocket.is_empty() {
        let websocket = spec
            .display_websocket
            .strip_prefix(endpoint::TCP)
            .unwrap_or(&spec.display_websocket);
        command.extend(strings(&["-display", "none"]));
        command.extend([
            "-object".into(),
            format!(
                "secret,id=beatbox-display-password,file={}",
                endpoint::option(&spec.display_password_file)
            ),
            "-vnc".into(),
            format!(
                "unix:{},websocket={},password-secret=beatbox-display-password",
                endpoint::option(&spec.display_socket),
                websocket
            ),
        ]);
        command.extend(pointer_args());
    } else {
        command.extend(pointer_args());
    }
    // Deliberately absent: -virtfs / -fsdev, a clipboard agent, usb-host.
    // Each is a hole through the boundary and none arrives by default.
    Ok(command)
}

// --- whether a window can be shown ---------------------------------------------------

/// How long `-display help` gets to answer.
pub const DISPLAY_HELP_WITHIN: Duration = Duration::from_secs(20);

/// Whether this QEMU build can put a window on the screen.
///
/// Some distributions package every display backend separately, so the base
/// package answers `-display help` with only `none` -- and a machine asked
/// for its window boots anyway, burns real CPU, and sits at a screen nobody
/// can click in.
pub fn display_available(qemu: &Qemu, runner: &dyn Runner) -> bool {
    if !qemu.system_found {
        return false;
    }
    let argv = vec![qemu.system.clone(), "-display".into(), "help".into()];
    // 20 s, not 5: on Windows it took over 5 with a window closing beside it,
    // and the install was refused as screenless. Asked once before an
    // hour-long install, so waiting costs nothing.
    match runner.run(&argv, Some(DISPLAY_HELP_WITHIN)) {
        Ok(output) => windowed_backends(&output.stdout),
        Err(_) => false,
    }
}

/// Whether QEMU can serve the machine window over VNC WebSocket (no GTK/SDL
/// needed). `-vnc help` exits 1 even when it works, so the output is read.
pub fn vnc_display_available(qemu: &Qemu, runner: &dyn Runner) -> bool {
    if !qemu.system_found {
        return false;
    }
    let argv = vec![qemu.system.clone(), "-vnc".into(), "help".into()];
    match runner.run(&argv, Some(DISPLAY_HELP_WITHIN)) {
        Ok(output) => {
            let options = format!("{}{}", output.stdout, output.stderr);
            options.contains("vnc options:")
                && options.contains("websocket=<")
                && options.contains("password-secret=<")
        }
        Err(_) => false,
    }
}

/// Whether `-display help`'s answer lists a windowed backend.
pub fn windowed_backends(listing: &str) -> bool {
    let mut listing_started = false;
    for line in listing.lines() {
        if line.starts_with("Available display backend types") {
            listing_started = true;
            continue;
        }
        if !listing_started {
            continue;
        }
        let name = line.trim();
        if name.is_empty() {
            break;
        }
        if WINDOWED.contains(&name) {
            return true;
        }
    }
    false
}

/// The one command that gives this system's QEMU a window, for each family
/// of Linux, and the two package names that cover almost everything else.
pub const DISPLAY_PACKAGES: [(&[&str], &str); 4] = [
    (&["debian", "ubuntu"], "sudo apt install qemu-system-gui"),
    (
        &["fedora", "rhel", "centos"],
        "sudo dnf install qemu-ui-gtk",
    ),
    (&["suse", "opensuse"], "sudo zypper install qemu-ui-gtk"),
    (&["arch"], "sudo pacman -S qemu-ui-gtk"),
];
/// See [`DISPLAY_PACKAGES`].
pub const UNKNOWN_DISPLAY_PACKAGES: &str = "qemu-ui-gtk / qemu-system-gui";

/// The one command that gives this system's QEMU a window.
///
/// Read from os-release rather than guessed from which package manager is
/// on `PATH`: a Fedora machine with apt installed for something else is
/// still a Fedora machine.
pub fn display_install_hint() -> String {
    display_install_hint_for(Path::new("/etc/os-release"), WINDOWS)
}

/// [`display_install_hint`] for an os-release file, on either system.
pub fn display_install_hint_for(os_release: &Path, windows: bool) -> String {
    if windows {
        return "https://qemu.weilnetz.de/w64/".into();
    }
    let Ok(text) = fs::read_to_string(os_release) else {
        return UNKNOWN_DISPLAY_PACKAGES.into();
    };
    let family = os_family(&text);
    for (names, command) in DISPLAY_PACKAGES {
        if family.iter().any(|member| names.contains(member)) {
            return command.into();
        }
    }
    UNKNOWN_DISPLAY_PACKAGES.into()
}

/// Which distributions an os-release file says this is: its `ID`, then
/// everything in `ID_LIKE`.
pub fn os_family(os_release: &str) -> Vec<&str> {
    let fields: Vec<(&str, &str)> = os_release
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let field = |key: &str| {
        fields
            .iter()
            .rev()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.trim_matches('"'))
            .unwrap_or_default()
    };
    let mut family: Vec<&str> = vec![field("ID")];
    family.extend(field("ID_LIKE").split_whitespace());
    family
}

// --- building a disk to run later ----------------------------------------------------

/// Whether a window can be shown, and what to install if not: what a disk
/// build is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    /// A windowed backend is there.
    pub available: bool,
    /// The one command that would give it one.
    pub install_hint: String,
}

impl Display {
    /// This host's.
    pub fn here(qemu: &Qemu, runner: &dyn Runner) -> Self {
        Self {
            available: display_available(qemu, runner),
            install_hint: display_install_hint(),
        }
    }
}

/// An install: a disc, a disk to put it on, and the room to do it in.
///
/// The one thing that is **not** disposable, so it's its own type and
/// builder instead of a flag on [`MachineSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskBuild {
    /// The installer disc.
    pub source: String,
    /// The disk to make and install onto.
    pub target: String,
    /// How big, sparse.
    pub size_gb: i64,
    /// `-m` for the installer.
    pub memory_mb: i64,
    /// `-smp` for the installer.
    pub cpus: i64,
    /// Whether the installer gets a way out.
    pub network: bool,
    /// Where an agent inside is reached.
    pub agent_socket: String,
    /// Where QMP listens.
    pub monitor_socket: String,
    /// A folder holding the installer's answers (`autounattend.xml`),
    /// plugged in as a USB drive, where Windows Setup looks for one. With
    /// answers there is nobody to click anything, so there is no window.
    pub answers: String,
    /// End at the guest's first reboot (`-no-reboot`): the installer is done
    /// copying, and what boots next -- the system's own first-boot setup --
    /// belongs to the machine made from this disk, not to the disk.
    pub ends_at_reboot: bool,
}

impl Default for DiskBuild {
    fn default() -> Self {
        Self {
            source: String::new(),
            target: String::new(),
            size_gb: 60,
            memory_mb: 4096,
            cpus: 2,
            network: false,
            agent_socket: String::new(),
            monitor_socket: String::new(),
            answers: String::new(),
            ends_at_reboot: false,
        }
    }
}

impl DiskBuild {
    /// Everything wrong with this build, in one pass.
    pub fn validate(&self, qemu: &Qemu, display: &Display) -> Vec<Reason> {
        let mut problems = Vec::new();
        if !qemu.image_found {
            problems.push(reason!("build.no_qemu_img", binary = QEMU_IMG));
        }
        if !display.available && self.answers.is_empty() {
            problems.push(reason!("build.no_display", install = display.install_hint));
        }
        if self.source.is_empty() {
            problems.push(reason!("build.no_source"));
        } else if !Path::new(&self.source).is_file() {
            problems.push(reason!("machine.no_such_image", path = self.source));
        } else if !is_optical(&self.source) {
            problems.push(reason!("build.source_not_a_disc", path = self.source));
        }
        if self.target.is_empty() {
            problems.push(reason!("build.no_target"));
        } else if Path::new(&self.target).exists() {
            problems.push(reason!("build.target_exists", path = self.target));
        }
        if self.size_gb < 1 {
            problems.push(reason!("build.too_small"));
        }
        problems
    }
}

fn detail_of(stderr: &str) -> String {
    stderr.trim().chars().take(200).collect()
}

/// Make the empty disk. Sparse, so 60G costs nothing until it is used.
pub fn create_disk(
    build: &DiskBuild,
    qemu: &Qemu,
    display: &Display,
    runner: &dyn Runner,
) -> Result<(), MachineError> {
    let problems = build.validate(qemu, display);
    if !problems.is_empty() {
        return Err(MachineError { reasons: problems });
    }
    if let Some(parent) = Path::new(&build.target).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let argv = vec![
        qemu.image.clone(),
        "create".into(),
        "-f".into(),
        DISK_FORMAT.into(),
        build.target.clone(),
        format!("{}G", build.size_gb),
    ];
    let output = runner
        .run(&argv, None)
        .map_err(|error| MachineError::new(reason!("build.create_failed", detail = error)))?;
    if output.code != 0 {
        return Err(MachineError::new(reason!(
            "build.create_failed",
            detail = detail_of(&output.stderr)
        )));
    }
    Ok(())
}

/// What QEMU thinks a file is -- `qcow2`, `raw` -- read, not guessed.
///
/// Needed because an overlay has to name its backing file's format: qemu-img
/// has refused to probe it since 6.1, for a good reason. A raw disk whose
/// first bytes look like a qcow2 header is a file somebody can hand you.
pub fn disk_format(path: &str, qemu: &Qemu, runner: &dyn Runner) -> Result<String, MachineError> {
    let argv = vec![
        qemu.image.clone(),
        "info".into(),
        "--output=json".into(),
        path.to_owned(),
    ];
    let unreadable = |detail: String| {
        MachineError::new(reason!(
            "build.unreadable_disk",
            path = path,
            detail = detail
        ))
    };
    let output = runner
        .run(&argv, None)
        .map_err(|e| unreadable(e.to_string()))?;
    if output.code != 0 {
        return Err(unreadable(detail_of(&output.stderr)));
    }
    let info: Value =
        serde_json::from_str(&output.stdout).map_err(|error| unreadable(error.to_string()))?;
    match info.get("format") {
        Some(Value::String(format)) => Ok(format.clone()),
        Some(other) => Ok(other.to_string()),
        None => Err(unreadable("'format'".into())),
    }
}

/// The save points inside a qcow2, by name, oldest first, or none if they
/// cannot be read.
///
/// Read off the disk, not kept in memory, so they survive BeatBox reopening.
/// `--force-share` because a running QEMU locks the disk (this only reads
/// the header); JSON because `info snapshots` is a table for people.
pub fn disk_snapshots(path: &str, qemu: &Qemu, runner: &dyn Runner) -> Vec<String> {
    let argv = vec![
        qemu.image.clone(),
        "info".into(),
        "--force-share".into(),
        "--output=json".into(),
        path.to_owned(),
    ];
    let Ok(output) = runner.run(&argv, None) else {
        return Vec::new();
    };
    if output.code != 0 {
        return Vec::new();
    }
    let Ok(info) = serde_json::from_str::<Value>(&output.stdout) else {
        return Vec::new();
    };
    info.get("snapshots")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| match entry.get("name") {
                    Some(Value::String(name)) if !name.is_empty() => Some(name.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Give a sandbox a disk of its own, backed by an image it cannot write.
///
/// The overlay holds every change; the base is read through it and never
/// opened for writing. Deleting the overlay is what "destroy this sandbox"
/// means for its disk, and remaking it is what "reset" means.
pub fn create_overlay(
    base: &str,
    target: &str,
    qemu: &Qemu,
    runner: &dyn Runner,
) -> Result<(), MachineError> {
    if !qemu.image_found {
        return Err(MachineError::new(reason!(
            "build.no_qemu_img",
            binary = QEMU_IMG
        )));
    }
    let source = Path::new(base);
    if !source.is_file() {
        return Err(MachineError::new(reason!(
            "machine.no_such_image",
            path = base
        )));
    }
    if Path::new(target).exists() {
        return Err(MachineError::new(reason!(
            "build.target_exists",
            path = target
        )));
    }
    if let Some(parent) = Path::new(target).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let resolved = crate::host_os::canonical(source).unwrap_or_else(|_| source.to_path_buf());
    let format = disk_format(base, qemu, runner)?;
    let argv = vec![
        qemu.image.clone(),
        "create".into(),
        "-f".into(),
        DISK_FORMAT.into(),
        "-b".into(),
        resolved.to_string_lossy().into_owned(),
        "-F".into(),
        format,
        target.to_owned(),
    ];
    let output = runner
        .run(&argv, None)
        .map_err(|error| MachineError::new(reason!("build.overlay_failed", detail = error)))?;
    if output.code != 0 {
        return Err(MachineError::new(reason!(
            "build.overlay_failed",
            detail = detail_of(&output.stderr)
        )));
    }
    Ok(())
}

/// The install argv itself, with no opinion about whether the target exists.
///
/// No `-snapshot` -- the one place that is absent, because the whole purpose
/// is to keep what the guest writes. Two media at once: the disc and the
/// disk. No card unless asked: Windows Setup offline skips the account it
/// otherwise insists on.
pub fn install_argv(build: &DiskBuild, qemu: &Qemu, uuid: &str) -> Vec<String> {
    let stem = Path::new(&build.target)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut command = vec![
        qemu.system.clone(),
        "-name".into(),
        format!("install:{stem}"),
        "-uuid".into(),
        uuid.to_owned(),
        "-m".into(),
        build.memory_mb.to_string(),
        "-smp".into(),
        build.cpus.to_string(),
        "-machine".into(),
        "q35".into(),
    ];
    command.extend(disk_args(&build.target, "sysdisk"));
    command.extend([
        "-cdrom".into(),
        endpoint::option(&build.source),
        "-boot".into(),
        "once=d".into(),
        "-nic".into(),
        if build.network { "user" } else { "none" }.into(),
    ]);
    command.extend(pointer_args());
    if build.ends_at_reboot {
        command.push("-no-reboot".into());
    }
    if !build.answers.is_empty() {
        command.extend([
            "-display".into(),
            "none".into(),
            "-blockdev".into(),
            format!(
                "driver=vvfat,node-name=answers,dir={},label=ANSWERS,read-only=on",
                endpoint::option(&build.answers)
            ),
            "-device".into(),
            "usb-storage,bus=usb0.0,drive=answers,removable=on".into(),
        ]);
    }
    if !build.monitor_socket.is_empty() {
        command.push("-qmp".into());
        command.push(endpoint::qmp_argument(&build.monitor_socket));
    }
    if !build.agent_socket.is_empty() {
        command.extend(agent_args(&build.agent_socket));
    }
    command.extend(accel_args(qemu));
    command.extend(balloon_args());
    command
}

/// The argv that installs the disc onto the disk, refusing to overwrite one.
///
/// Validates in full, `build.target_exists` included: the target is presumed
/// somebody's real machine. [`start_install`] uses [`install_argv`] instead,
/// since after [`create_disk`] the target exists and this would refuse it.
pub fn build_install_command(
    build: &DiskBuild,
    qemu: &Qemu,
    display: &Display,
    uuid: &str,
) -> Result<Vec<String>, MachineError> {
    let problems = build.validate(qemu, display);
    if !problems.is_empty() {
        return Err(MachineError { reasons: problems });
    }
    Ok(install_argv(build, qemu, uuid))
}

// --- running one -----------------------------------------------------------------------

/// A guest that is up, and the handle to stop it.
pub struct RunningMachine {
    /// What it was started as.
    pub spec: MachineSpec,
    /// The QEMU process.
    pub child: Child,
    /// Whether it runs on a hypervisor.
    pub accelerated: bool,
    exited: Option<ExitStatus>,
}

impl RunningMachine {
    /// Wrap a QEMU process this process started.
    pub fn new(spec: MachineSpec, child: Child, accelerated: bool) -> Self {
        Self {
            spec,
            child,
            accelerated,
            exited: None,
        }
    }

    /// Whether QEMU is still there. Reaps it if it is not.
    pub fn running(&mut self) -> bool {
        self.exit_status().is_none()
    }

    /// How it ended, if it has.
    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        if self.exited.is_none() {
            self.exited = self.child.try_wait().ok().flatten();
        }
        self.exited
    }

    /// Ask it to go, then insist: SIGTERM, a wait of `timeout`, then SIGKILL.
    pub fn stop(&mut self, timeout: Duration) -> Option<ExitStatus> {
        if self.running() {
            terminate(&mut self.child);
            if self.wait(timeout).is_none() {
                let _ = self.child.kill();
                self.wait(timeout);
            }
        }
        self.exit_status()
    }

    /// Wait up to `timeout` for it to end.
    pub fn wait(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.exit_status() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The polite half of stopping: SIGTERM. Windows has none; there
/// [`Child::kill`] is `TerminateProcess`.
#[cfg(unix)]
#[allow(unsafe_code)] // kill(2): the one libc call std has no safe wrapper for.
fn terminate(child: &mut Child) {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    const SIGTERM: i32 = 15;
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    // Safe: `pid` is our own child, not yet reaped (`running()` just said so),
    // so the number cannot belong to anybody else; kill(2) takes no pointers.
    unsafe {
        kill(pid, SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate(child: &mut Child) {
    let _ = child.kill();
}

/// Bring the guest up.
///
/// stdout to nowhere, stderr to the log file: an unread pipe fills at 64 KiB,
/// then QEMU blocks on a warning and the guest freezes.
pub fn start_machine(
    spec: &MachineSpec,
    qemu: &Qemu,
    uuid: &str,
) -> Result<RunningMachine, MachineError> {
    if !qemu.system_found {
        return Err(MachineError::new(reason!(
            "machine.no_qemu",
            binary = QEMU_BINARY
        )));
    }
    let command = build_command(spec, qemu, uuid)?;
    let stderr = if spec.log_path.is_empty() {
        Stdio::null()
    } else {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spec.log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null())
    };
    let child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|error| {
            MachineError::new(reason!(
                "machine.could_not_start",
                binary = QEMU_BINARY,
                detail = error,
            ))
        })?;
    Ok(RunningMachine::new(spec.clone(), child, qemu.accelerated()))
}

/// Everything [`start_install`] does before spawning QEMU: refuse without
/// QEMU, make the empty disk (validated first), build the argv. Apart so a
/// caller that spawns its own way (the facade's tests) keeps this order.
pub fn prepare_install(
    build: &DiskBuild,
    qemu: &Qemu,
    display: &Display,
    runner: &dyn Runner,
    uuid: &str,
) -> Result<Vec<String>, MachineError> {
    if !qemu.system_found {
        return Err(MachineError::new(reason!(
            "machine.no_qemu",
            binary = QEMU_BINARY
        )));
    }
    create_disk(build, qemu, display, runner)?;
    Ok(install_argv(build, qemu, uuid))
}

/// Create the disk and boot the installer onto it. Deliberately visible:
/// somebody has to click through Setup.
pub fn start_install(
    build: &DiskBuild,
    qemu: &Qemu,
    display: &Display,
    runner: &dyn Runner,
    uuid: &str,
) -> Result<RunningMachine, MachineError> {
    let command = prepare_install(build, qemu, display, runner, uuid)?;
    let child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            MachineError::new(reason!(
                "machine.could_not_start",
                binary = QEMU_BINARY,
                detail = error,
            ))
        })?;
    let stem = Path::new(&build.target)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let spec = MachineSpec {
        name: stem,
        image: build.target.clone(),
        ..MachineSpec::default()
    };
    Ok(RunningMachine::new(spec, child, qemu.accelerated()))
}
