//! How a machine starts: the PC BIOS, or UEFI.
//!
//! QEMU starts every guest with SeaBIOS unless it is handed a UEFI firmware
//! -- OVMF, from the edk2 project -- as two flash chips: the firmware's code,
//! read-only and shared, and a store of UEFI variables that is the machine's
//! own. A disk installed under UEFI does not boot under BIOS, which is every
//! Windows 11 and many a recent Linux, and every VirtualBox machine that had
//! "Enable EFI" ticked.
//!
//! **OVMF is found through QEMU's firmware descriptors** in
//! `share/qemu/firmware` (what libvirt reads), falling back to QEMU's own
//! `share/edk2-*`, which is where QEMU for Windows keeps it. Plain builds
//! only: Secure Boot also needs SMM and enrolled keys.
//!
//! **The disk says which one it wants:** GPT with an EFI system partition and
//! no BIOS boot partition is UEFI; anything else, both included, is BIOS.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::machine::{Qemu, os_family};
use crate::runner::Runner;

/// UEFI, as a spec asks for it.
pub const UEFI: &str = "uefi";
/// The PC BIOS: QEMU's default, SeaBIOS.
pub const BIOS: &str = "bios";

/// Where each distribution puts QEMU's firmware descriptors.
pub const DESCRIPTOR_FOLDERS: [&str; 3] = [
    "/etc/qemu/firmware",
    "/usr/share/qemu/firmware",
    "/usr/local/share/qemu/firmware",
];

/// Known pairs, for a system whose OVMF came without descriptors.
pub const KNOWN_PAIRS: [(&str, &str); 3] = [
    (
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/OVMF/OVMF_VARS_4M.fd",
    ),
    (
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_VARS.fd",
    ),
    (
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/OVMF_VARS.fd",
    ),
];

/// A UEFI firmware: its code, and the variable store every machine starts
/// its own from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uefi {
    /// The firmware itself, mapped read-only.
    pub code: PathBuf,
    /// An empty variable store, copied once per machine.
    pub vars_template: PathBuf,
}

/// This computer's OVMF, or `None`.
pub fn find_uefi(qemu: &Qemu) -> Option<Uefi> {
    let mut folders: Vec<PathBuf> = DESCRIPTOR_FOLDERS.iter().map(PathBuf::from).collect();
    let mut pairs: Vec<(PathBuf, PathBuf)> = KNOWN_PAIRS
        .iter()
        .map(|(code, vars)| (PathBuf::from(code), PathBuf::from(vars)))
        .collect();
    // QEMU for Windows, and any QEMU found by its own path: `bin/../share`
    // or, on Windows, `share` beside the executable.
    if let Some(bin) = Path::new(&qemu.system).parent() {
        for share in [bin.join("share"), bin.join("../share")] {
            folders.push(share.join("firmware"));
            pairs.push((
                share.join("edk2-x86_64-code.fd"),
                share.join("edk2-i386-vars.fd"),
            ));
        }
    }
    find_uefi_in(&folders, &pairs)
}

/// [`find_uefi`], looking only where it is told to.
pub fn find_uefi_in(folders: &[PathBuf], pairs: &[(PathBuf, PathBuf)]) -> Option<Uefi> {
    for folder in folders {
        let Ok(entries) = fs::read_dir(folder) else {
            continue;
        };
        let mut descriptors: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        // The numbers in front are the priority the descriptors ask for.
        descriptors.sort();
        for descriptor in descriptors {
            if let Some(found) = descriptor_uefi(&descriptor) {
                return Some(found);
            }
        }
    }
    pairs
        .iter()
        .find(|(code, vars)| code.is_file() && vars.is_file())
        .map(|(code, vars)| Uefi {
            code: code.clone(),
            vars_template: vars.clone(),
        })
}

/// One descriptor, if it is plain x86_64 UEFI on flash for the `pc` chipset
/// and its two files are really there. A file named with the prefix QEMU
/// was built for rather than installed at is looked for beside the
/// descriptor's folder too, which is how QEMU for Windows ships them.
fn descriptor_uefi(descriptor: &Path) -> Option<Uefi> {
    let text = fs::read_to_string(descriptor).ok()?;
    let found: Value = serde_json::from_str(&text).ok()?;
    let listed = |key: &str| -> Vec<&str> {
        found[key]
            .as_array()
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    };
    if !listed("interface-types").contains(&"uefi") || found["mapping"]["device"] != "flash" {
        return None;
    }
    let features = listed("features");
    if [
        "secure-boot",
        "requires-smm",
        "enrolled-keys",
        "amd-sev-es",
        "intel-tdx",
    ]
    .iter()
    .any(|needs| features.contains(needs))
    {
        return None;
    }
    let for_pc = found["targets"].as_array()?.iter().any(|target| {
        target["architecture"] == "x86_64"
            && target["machines"].as_array().is_some_and(|machines| {
                machines
                    .iter()
                    .any(|m| m.as_str().is_some_and(|m| m.starts_with("pc-i440fx")))
            })
    });
    if !for_pc {
        return None;
    }
    let share = descriptor.parent()?.parent()?;
    let file = |key: &str| -> Option<PathBuf> {
        let named = PathBuf::from(found["mapping"][key]["filename"].as_str()?);
        if named.is_file() {
            return Some(named);
        }
        let beside = share.join(named.file_name()?);
        beside.is_file().then_some(beside)
    };
    Some(Uefi {
        code: file("executable")?,
        vars_template: file("nvram-template")?,
    })
}

/// How many sectors are read to decide: the MBR, the GPT header and 128
/// partition entries of 128 bytes.
pub const HEAD_SECTORS: usize = 34;

/// The EFI system partition's type, as GPT stores it on disk.
const ESP: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
/// GRUB's BIOS boot partition, which is what lets BIOS boot a GPT disk.
const BIOS_BOOT: [u8; 16] = [
    0x48, 0x61, 0x68, 0x21, 0x49, 0x64, 0x6F, 0x6E, 0x74, 0x4E, 0x65, 0x65, 0x64, 0x45, 0x46, 0x49,
];

/// Which firmware the first sectors of a disk were installed for.
pub fn firmware_of(head: &[u8]) -> &'static str {
    if head.len() < 1024 || head[510..512] != [0x55, 0xAA] || head[446 + 4] != 0xEE {
        return BIOS;
    }
    if &head[512..520] != b"EFI PART" {
        return BIOS;
    }
    let number = |at: usize, width: usize| -> usize {
        head[at..at + width]
            .iter()
            .rev()
            .fold(0usize, |value, byte| value << 8 | usize::from(*byte))
    };
    let first = number(512 + 72, 8).saturating_mul(512);
    let count = number(512 + 80, 4);
    let size = number(512 + 84, 4);
    if size < 16 {
        return BIOS;
    }
    let (mut esp, mut bios_boot) = (false, false);
    for index in 0..count {
        let at = first + index * size;
        let Some(kind) = head.get(at..at + 16) else {
            break;
        };
        esp |= kind == ESP;
        bios_boot |= kind == BIOS_BOOT;
    }
    if esp && !bios_boot { UEFI } else { BIOS }
}

/// The first sectors of a disk image, read through `qemu-img dd` into
/// `scratch` (`dd` only writes to files), or `None` if unreadable.
pub fn disk_head(
    image: &str,
    format: &str,
    qemu: &Qemu,
    runner: &dyn Runner,
    scratch: &Path,
) -> Option<Vec<u8>> {
    let argv = vec![
        qemu.image.clone(),
        "dd".into(),
        "-f".into(),
        format.to_owned(),
        "-O".into(),
        "raw".into(),
        "bs=512".into(),
        format!("count={HEAD_SECTORS}"),
        format!("if={image}"),
        format!("of={}", scratch.display()),
    ];
    let _ = fs::remove_file(scratch);
    let read = runner
        .run(&argv, None)
        .ok()
        .filter(|output| output.code == 0)
        .and_then(|_| fs::read(scratch).ok());
    let _ = fs::remove_file(scratch);
    read
}

/// Whether anything on a disk can start: the boot signature at the end of
/// its first sector, which an MBR has and a GPT's protective MBR has too. A
/// disk nothing was installed on has neither, and QEMU would try the
/// network and then say "No bootable device".
pub fn bootable(head: &[u8]) -> bool {
    head.len() >= 512 && head[510..512] == [0x55, 0xAA]
}

/// Nothing written at all: the first sectors are zeros.
pub fn blank(head: &[u8]) -> bool {
    head.iter().all(|&byte| byte == 0)
}

/// How a disk starts: which firmware it was installed for, and whether it
/// can start at all. Unreadable means BIOS and bootable; QEMU will say if not.
pub fn disk_start(
    image: &str,
    format: &str,
    qemu: &Qemu,
    runner: &dyn Runner,
    scratch: &Path,
) -> (&'static str, bool) {
    disk_head(image, format, qemu, runner, scratch)
        .map_or((BIOS, true), |head| (firmware_of(&head), bootable(&head)))
}

/// Which firmware a disk image wants. See [`disk_start`].
pub fn disk_firmware(
    image: &str,
    format: &str,
    qemu: &Qemu,
    runner: &dyn Runner,
    scratch: &Path,
) -> &'static str {
    disk_start(image, format, qemu, runner, scratch).0
}

/// The command that installs OVMF here, read from os-release.
pub fn uefi_install_hint_for(os_release: &str, windows: bool) -> String {
    if windows {
        return "https://qemu.weilnetz.de/w64/".into();
    }
    let family = os_family(os_release);
    let is = |names: &[&str]| family.iter().any(|member| names.contains(member));
    if is(&["debian", "ubuntu"]) {
        "sudo apt install ovmf".into()
    } else if is(&["fedora", "rhel", "centos"]) {
        "sudo dnf install edk2-ovmf".into()
    } else if is(&["suse", "opensuse"]) {
        "sudo zypper install qemu-ovmf-x86_64".into()
    } else if is(&["arch"]) {
        "sudo pacman -S edk2-ovmf".into()
    } else {
        "ovmf / edk2-ovmf".into()
    }
}

/// [`uefi_install_hint_for`] on this computer.
pub fn uefi_install_hint() -> String {
    uefi_install_hint_for(
        &fs::read_to_string("/etc/os-release").unwrap_or_default(),
        crate::host_os::WINDOWS,
    )
}
