//! Which firmware a disk wants, and where this computer's UEFI is.

mod support;

use std::path::PathBuf;
use std::process::Command;

use beatbox_isolation::firmware::{
    BIOS, UEFI, disk_firmware, find_uefi_in, firmware_of, uefi_install_hint_for,
};
use beatbox_isolation::machine::Qemu;
use beatbox_isolation::runner::SystemRunner;
use support::Room;

const ESP: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
const BIOS_BOOT: [u8; 16] = [
    0x48, 0x61, 0x68, 0x21, 0x49, 0x64, 0x6F, 0x6E, 0x74, 0x4E, 0x65, 0x65, 0x64, 0x45, 0x46, 0x49,
];
const LINUX_DATA: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];

/// The first 34 sectors of a GPT disk with these partition types.
fn gpt(kinds: &[[u8; 16]]) -> Vec<u8> {
    let mut head = vec![0u8; 34 * 512];
    head[446 + 4] = 0xEE;
    head[510] = 0x55;
    head[511] = 0xAA;
    head[512..520].copy_from_slice(b"EFI PART");
    head[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
    head[512 + 80..512 + 84].copy_from_slice(&128u32.to_le_bytes());
    head[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
    for (index, kind) in kinds.iter().enumerate() {
        let at = 1024 + index * 128;
        head[at..at + 16].copy_from_slice(kind);
    }
    head
}

#[test]
fn a_gpt_disk_with_an_efi_partition_and_no_bios_boot_is_uefi() {
    assert_eq!(firmware_of(&gpt(&[ESP, LINUX_DATA])), UEFI);
}

/// GRUB installed for BIOS on GPT keeps a BIOS boot partition: BIOS boots
/// it, and BIOS is what BeatBox always did.
#[test]
fn a_gpt_disk_that_bios_can_boot_stays_bios() {
    assert_eq!(firmware_of(&gpt(&[BIOS_BOOT, ESP, LINUX_DATA])), BIOS);
    assert_eq!(firmware_of(&gpt(&[LINUX_DATA])), BIOS);
}

#[test]
fn an_mbr_disk_or_nothing_at_all_is_bios() {
    let mut mbr = vec![0u8; 34 * 512];
    mbr[446 + 4] = 0x07;
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    assert_eq!(firmware_of(&mbr), BIOS);
    assert_eq!(firmware_of(&[]), BIOS);
    assert_eq!(firmware_of(&vec![0u8; 34 * 512]), BIOS);
}

/// Read through QEMU, so a VirtualBox disk is read like any other.
#[test]
fn it_is_read_off_a_real_vdi() {
    let qemu = Qemu::here();
    if !qemu.image_found {
        eprintln!("SKIPPED: no qemu-img");
        return;
    }
    let room = Room::new("firmware");
    let raw = room.join("disk.raw");
    let mut bytes = gpt(&[ESP, LINUX_DATA]);
    bytes.resize(4 << 20, 0);
    std::fs::write(&raw, &bytes).unwrap();
    let vdi = room.join("disk.vdi");
    let made = Command::new(&qemu.image)
        .args(["convert", "-f", "raw", "-O", "vdi"])
        .arg(&raw)
        .arg(&vdi)
        .status()
        .unwrap();
    assert!(made.success());
    let scratch = room.join("head");
    let found = disk_firmware(
        &vdi.to_string_lossy(),
        "vdi",
        &qemu,
        &SystemRunner,
        &scratch,
    );
    assert_eq!(found, UEFI);
    assert!(!scratch.exists(), "the scratch copy is not left behind");
}

/// A path, escaped for embedding in a JSON string literal: on Windows it has
/// backslashes of its own, which the descriptor's JSON would otherwise take
/// as escapes.
fn text(room: &Room, relative: &str) -> String {
    room.join(relative).to_string_lossy().replace('\\', "\\\\")
}

fn descriptor(room: &Room, name: &str, features: &str, code: &str, vars: &str) {
    std::fs::create_dir_all(room.join("share/firmware")).unwrap();
    std::fs::write(
        room.join(&format!("share/firmware/{name}")),
        format!(
            r#"{{"interface-types": ["uefi"],
                "mapping": {{"device": "flash",
                            "executable": {{"filename": "{code}", "format": "raw"}},
                            "nvram-template": {{"filename": "{vars}", "format": "raw"}}}},
                "targets": [{{"architecture": "x86_64", "machines": ["pc-i440fx-*", "pc-q35-*"]}}],
                "features": [{features}]}}"#
        ),
    )
    .unwrap();
}

/// Secure Boot needs SMM and enrolled keys, which BeatBox does not set up:
/// a descriptor asking for them is passed over for a plain one.
#[test]
fn a_plain_uefi_descriptor_is_chosen_over_a_secure_one() {
    let room = Room::new("firmware");
    for file in ["secure-code.fd", "secure-vars.fd", "code.fd", "vars.fd"] {
        std::fs::write(room.join(file), b"").unwrap();
    }
    descriptor(
        &room,
        "40-secure.json",
        r#""secure-boot", "requires-smm""#,
        &text(&room, "secure-code.fd"),
        &text(&room, "secure-vars.fd"),
    );
    descriptor(
        &room,
        "60-plain.json",
        r#""acpi-s3""#,
        &text(&room, "code.fd"),
        &text(&room, "vars.fd"),
    );
    let found = find_uefi_in(&[room.join("share/firmware")], &[]).unwrap();
    assert_eq!(found.code, room.join("code.fd"));
    assert_eq!(found.vars_template, room.join("vars.fd"));
}

/// QEMU for Windows writes the prefix it was built for into its
/// descriptors; the files are found beside them instead.
#[test]
fn a_descriptor_whose_paths_are_elsewhere_is_found_beside_itself() {
    let room = Room::new("firmware");
    std::fs::create_dir_all(room.join("share")).unwrap();
    std::fs::write(room.join("share/edk2-x86_64-code.fd"), b"").unwrap();
    std::fs::write(room.join("share/edk2-i386-vars.fd"), b"").unwrap();
    descriptor(
        &room,
        "60-edk2-x86_64.json",
        "",
        "/nowhere/share/edk2-x86_64-code.fd",
        "/nowhere/share/edk2-i386-vars.fd",
    );
    let found = find_uefi_in(&[room.join("share/firmware")], &[]).unwrap();
    assert_eq!(found.code, room.join("share/edk2-x86_64-code.fd"));
}

#[test]
fn no_firmware_anywhere_is_none() {
    let room = Room::new("firmware");
    let pair = (
        PathBuf::from("/nowhere/code.fd"),
        PathBuf::from("/nowhere/vars.fd"),
    );
    assert!(find_uefi_in(&[room.join("share/firmware")], &[pair]).is_none());
}

#[test]
fn the_install_command_is_this_systems() {
    assert_eq!(
        uefi_install_hint_for("ID=arch\n", false),
        "sudo pacman -S edk2-ovmf"
    );
    assert_eq!(
        uefi_install_hint_for("ID=ubuntu\nID_LIKE=debian\n", false),
        "sudo apt install ovmf"
    );
    assert_eq!(
        uefi_install_hint_for("ID=fedora\n", false),
        "sudo dnf install edk2-ovmf"
    );
}

#[test]
fn a_disk_starts_only_with_a_boot_signature() {
    use beatbox_isolation::firmware::{blank, bootable};
    assert!(bootable(&gpt(&[ESP])));
    let zeros = vec![0u8; 34 * 512];
    assert!(!bootable(&zeros) && blank(&zeros));
    let mut data = zeros.clone();
    data[4096] = 7;
    assert!(!bootable(&data) && !blank(&data));
}
