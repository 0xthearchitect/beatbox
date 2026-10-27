//! Machines left behind by an earlier run, and the sweep that removes them.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::net::TcpListener;

use support::{Room, open};

fn nothing_claimed() -> BTreeSet<std::path::PathBuf> {
    BTreeSet::new()
}

/// Windows: the channel is a port on 127.0.0.1, recorded in the same file. A
/// QEMU still listening on it holds the room.
#[test]
fn test_a_room_whose_recorded_port_answers_is_left_alone() {
    let room = Room::new("sweep");
    let api = open(room.path());
    let machine = room.join("machines/sbx-dddddddddddd");
    fs::create_dir_all(&machine).unwrap();
    let live = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = live.local_addr().unwrap().port();
    fs::write(
        machine.join("monitor.sock"),
        format!("tcp:127.0.0.1:{port}"),
    )
    .unwrap();
    api.sweep_leftovers(&nothing_claimed());
    assert!(machine.exists());
}

#[test]
fn test_a_room_whose_recorded_port_is_dead_is_removed() {
    let room = Room::new("sweep");
    let api = open(room.path());
    let machine = room.join("machines/sbx-eeeeeeeeeeee");
    fs::create_dir_all(&machine).unwrap();
    let port = {
        let gone = TcpListener::bind("127.0.0.1:0").unwrap();
        gone.local_addr().unwrap().port()
    };
    fs::write(
        machine.join("monitor.sock"),
        format!("tcp:127.0.0.1:{port}"),
    )
    .unwrap();
    api.sweep_leftovers(&nothing_claimed());
    assert!(!machine.exists());
}

#[cfg(unix)]
#[test]
fn test_a_room_nobody_holds_is_removed_wherever_it_was_kept() {
    use std::os::unix::net::UnixListener;
    let room = Room::new("sweep");
    let api = open(room.path());
    let here = room.join("machines/sbx-aaaaaaaaaaaa");
    let there = room.join("tmp/beatbox-host-sbx-bbbbbbbbbbbb");
    for machine in [&here, &there] {
        fs::create_dir_all(machine).unwrap();
        fs::write(machine.join("disk.qcow2"), b"what a guest wrote").unwrap();
    }
    // A socket file with nothing listening: what a killed QEMU leaves.
    drop(UnixListener::bind(here.join("monitor.sock")).unwrap());
    api.sweep_leftovers(&nothing_claimed());
    assert!(!here.exists());
    assert!(!there.exists());
}

#[cfg(unix)]
#[test]
fn test_a_room_another_beatbox_is_running_is_left_alone() {
    use std::os::unix::net::UnixListener;
    let room = Room::new("sweep");
    let api = open(room.path());
    let machine = room.join("machines/sbx-cccccccccccc");
    fs::create_dir_all(&machine).unwrap();
    let _live = UnixListener::bind(machine.join("monitor.sock")).unwrap();
    api.sweep_leftovers(&nothing_claimed());
    assert!(machine.exists());
}

#[test]
fn test_nothing_that_is_not_a_machine_room_is_touched() {
    let room = Room::new("sweep");
    let api = open(room.path());
    let other = room.join("tmp/somebody-elses");
    fs::create_dir_all(&other).unwrap();
    api.sweep_leftovers(&nothing_claimed());
    assert!(other.exists());
}

#[test]
fn test_a_new_machine_is_given_a_room_on_the_disk() {
    let room = Room::new("sweep");
    let api = open(room.path());
    let given = api.workspace();
    assert!(given.starts_with(&room.text("machines")), "{given}");
    let name = std::path::Path::new(&given)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name.starts_with("sbx-") && name.len() == 16, "{name}");
}

/// Settings' Storage: what a sweep would clear is counted, and clearing it
/// on demand is the same sweep.
#[cfg(unix)]
#[test]
fn test_storage_counts_what_is_left_and_clears_it_on_demand() {
    let room = Room::new("sweep");
    let api = open(room.path());
    let machine = room.join("machines/sbx-ffffffffffff");
    fs::create_dir_all(&machine).unwrap();
    fs::write(machine.join("disk.qcow2"), vec![7u8; 64 * 1024]).unwrap();
    fs::create_dir_all(room.join("disks")).unwrap();
    fs::write(room.join("disks/spare.qcow2"), vec![7u8; 4096]).unwrap();

    let seen = support::call(&api, "storage.usage", serde_json::json!({}));
    let seen = seen.data.unwrap();
    assert!(seen["leftoverBytes"].as_i64().unwrap() >= 64 * 1024);
    assert_eq!(seen["disks"][0]["name"], "spare");
    assert_eq!(seen["disks"][0]["madeInto"], serde_json::json!([]));

    let after = support::call(&api, "storage.sweep", serde_json::json!({}));
    assert!(after.ok, "{:?}", after.error);
    assert_eq!(after.data.unwrap()["leftoverBytes"], 0);
    assert!(!machine.exists());
    assert!(room.join("disks/spare.qcow2").exists());
}
