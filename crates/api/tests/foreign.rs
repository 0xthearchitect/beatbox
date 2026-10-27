//! Disks another program made: what BeatBox says about them before a
//! machine is made of one, and the disk taken out of an `.ova`.

mod support;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use beatbox_api::foreign::tar_entries;
use beatbox_api::{Api, ApiParts, Installer};
use beatbox_contract::DiskInspectView;
use beatbox_isolation::machine::Qemu;
use beatbox_isolation::runner::SystemRunner;
use serde_json::json;
use support::{Ran, Room, call, code, data, job, parts, settle};

fn api(room: &Room) -> Api {
    Api::open(ApiParts {
        installer: Installer {
            qemu: Arc::new(Qemu::here),
            runner: Arc::new(SystemRunner),
            ..support::installer(Arc::new(Ran::default()), true)
        },
        ..parts(room.path())
    })
}

fn qemu_img(arguments: &[&str]) -> bool {
    Command::new("qemu-img")
        .args(arguments)
        .status()
        .is_ok_and(|s| s.success())
}

fn inspect(api: &Api, path: &Path) -> DiskInspectView {
    serde_json::from_value(data(call(
        api,
        "disks.inspect",
        json!({"path": path.to_string_lossy()}),
    )))
    .unwrap()
}

/// What VirtualBox exports: the description first, then a stream-optimised
/// VMDK, in a plain tar.
fn ova(room: &Room) -> std::path::PathBuf {
    let made = room.join("export");
    fs::create_dir_all(&made).unwrap();
    fs::write(made.join("machine.ovf"), "<Envelope/>").unwrap();
    assert!(qemu_img(&[
        "create",
        "-q",
        "-f",
        "vmdk",
        "-o",
        "subformat=streamOptimized",
        &made.join("machine-disk001.vmdk").to_string_lossy(),
        "64M",
    ]));
    let archive = room.join("machine.ova");
    let packed = Command::new("tar")
        .arg("-cf")
        .arg(&archive)
        .arg("-C")
        .arg(&made)
        .args(["machine.ovf", "machine-disk001.vmdk"])
        .status()
        .unwrap();
    assert!(packed.success());
    archive
}

#[test]
fn a_vdi_is_read_as_it_is() {
    if !Qemu::here().image_found {
        return;
    }
    let room = Room::new("foreign");
    let api = api(&room);
    let vdi = room.join("vm/machine.vdi");
    fs::create_dir_all(vdi.parent().unwrap()).unwrap();
    assert!(qemu_img(&[
        "create",
        "-q",
        "-f",
        "vdi",
        &vdi.to_string_lossy(),
        "64M"
    ]));
    let seen = inspect(&api, &vdi);
    assert_eq!(seen.format, "vdi");
    assert_eq!(seen.bytes, 64 << 20);
    assert_eq!(seen.firmware, "bios");
    assert!(!seen.bootable, "nothing was ever installed on a fresh disk");
    assert!(!seen.virtualbox_snapshots);
    assert!(seen.problem.is_none());
}

/// VirtualBox keeps a machine's current state in difference files in
/// `Snapshots/` beside the disk; the disk alone is the machine before them.
#[test]
fn virtualbox_snapshots_beside_a_disk_are_said() {
    if !Qemu::here().image_found {
        return;
    }
    let room = Room::new("foreign");
    let api = api(&room);
    let vdi = room.join("vm/machine.vdi");
    fs::create_dir_all(room.join("vm/Snapshots")).unwrap();
    assert!(qemu_img(&[
        "create",
        "-q",
        "-f",
        "vdi",
        &vdi.to_string_lossy(),
        "64M"
    ]));
    fs::write(room.join("vm/Snapshots/{3f2a}.vdi"), b"").unwrap();
    assert!(inspect(&api, &vdi).virtualbox_snapshots);
}

#[test]
fn a_disk_that_is_not_there_says_so() {
    let room = Room::new("foreign");
    let seen = inspect(&api(&room), &room.join("nowhere.vmdk"));
    assert_eq!(seen.problem.unwrap().code, "disks.not_there");
}

#[test]
fn the_files_in_an_ova_are_found_with_where_they_are() {
    if !Qemu::here().image_found {
        return;
    }
    let room = Room::new("foreign");
    let archive = ova(&room);
    let entries = tar_entries(&mut fs::File::open(&archive).unwrap()).unwrap();
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["machine.ovf", "machine-disk001.vmdk"]);
    let disk = &entries[1];
    let original = fs::read(room.join("export/machine-disk001.vmdk")).unwrap();
    assert_eq!(disk.size, original.len() as u64);
    let bytes = fs::read(&archive).unwrap();
    assert_eq!(
        &bytes[disk.offset as usize..(disk.offset + disk.size) as usize],
        &original[..]
    );
}

/// Out of the archive into the disks folder, byte for byte, listed there
/// and forgotten from there like any other disk.
#[test]
fn the_disk_in_an_ova_is_taken_out_once() {
    if !Qemu::here().image_found {
        return;
    }
    let room = Room::new("foreign");
    let api = api(&room);
    let archive = ova(&room);
    let seen = inspect(&api, &archive);
    assert_eq!(seen.format, "ova");
    assert_eq!(seen.inside, "machine-disk001.vmdk");
    let started = job(call(
        &api,
        "disks.unpack",
        json!({"source": archive.to_string_lossy(), "name": "from-vbox"}),
    ));
    let done = settle(&api, &started.id, Duration::from_secs(10));
    assert_eq!(done.state, beatbox_api::jobs::DONE, "{:?}", done.reason);
    let target = Path::new(&done.message);
    assert_eq!(
        fs::read(target).unwrap(),
        fs::read(room.join("export/machine-disk001.vmdk")).unwrap()
    );
    let listed = data(call(&api, "disks.list", json!({})));
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|disk| disk["name"] == "from-vbox")
    );
    let again = call(
        &api,
        "disks.unpack",
        json!({"source": archive.to_string_lossy(), "name": "from-vbox"}),
    );
    assert_eq!(code(&again), "disk-exists");
    call(&api, "disks.forget", json!({"name": "from-vbox"}));
    assert!(!target.exists());
}

#[test]
fn an_ova_with_no_disk_in_it_says_so() {
    let room = Room::new("foreign");
    let api = api(&room);
    let made = room.join("export");
    fs::create_dir_all(&made).unwrap();
    fs::write(made.join("machine.ovf"), "<Envelope/>").unwrap();
    fs::write(made.join("machine-disk001.vmdk.gz"), "not really").unwrap();
    let archive = room.join("zipped.ova");
    Command::new("tar")
        .arg("-cf")
        .arg(&archive)
        .arg("-C")
        .arg(&made)
        .args(["machine.ovf", "machine-disk001.vmdk.gz"])
        .status()
        .unwrap();
    assert_eq!(
        inspect(&api, &archive).problem.unwrap().code,
        "disks.ova_compressed"
    );
    fs::remove_file(made.join("machine-disk001.vmdk.gz")).unwrap();
    let archive = room.join("empty.ova");
    Command::new("tar")
        .arg("-cf")
        .arg(&archive)
        .arg("-C")
        .arg(&made)
        .arg("machine.ovf")
        .status()
        .unwrap();
    assert_eq!(
        inspect(&api, &archive).problem.unwrap().code,
        "disks.ova_without_disk"
    );
}
