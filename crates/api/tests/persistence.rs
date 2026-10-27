//! A sandbox surviving BeatBox closing and opening again.
//!
//! Dropping the `Api` is BeatBox closing; opening another on the same
//! folders is it coming back. The mock throughout: no question here needs
//! a QEMU.

mod support;

use std::fs;

use beatbox_api::{Api, ApiParts};
use beatbox_engine::runtime::resources::CPU;
use beatbox_engine::{Lifecycle, Resources, SandboxConfig, data};
use serde_json::Value;
use support::{Room, capture_logs, logged, open, parts};

fn record(room: &Room, sandbox_id: &str) -> Value {
    let text = fs::read_to_string(room.join(&format!("state/{sandbox_id}.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn alice() -> SandboxConfig {
    SandboxConfig::new("mock").unwrap().named("alice")
}

/// An `Api` that knows no types at all: a build where a provider is missing.
fn without_types(room: &Room) -> Api {
    Api::open(ApiParts {
        types: Vec::new(),
        ..parts(room.path())
    })
}

#[test]
fn test_creating_one_writes_it_down() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    let written = record(&room, &made.id());
    assert_eq!(written["identity"]["name"], "alice");
    assert_eq!(written["config"]["type"], "mock");
    assert_eq!(written["lifecycle"], "created");
}

#[test]
fn test_a_lifecycle_change_is_written_down_again() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.start().unwrap();
    assert_eq!(record(&room, &made.id())["lifecycle"], "running");
}

/// `perform` fires `sandbox.action` on every call: a command run inside a
/// guest must not cost a write each time.
#[test]
fn test_an_action_writes_nothing() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.start().unwrap();
    let path = room.join(&format!("state/{}.json", made.id()));
    let written_at = fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    made.act("process.execute", data!({"command": "/bin/true"}))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), written_at);
}

#[test]
fn test_a_new_configuration_is_written_down() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.reconfigure(
        SandboxConfig::new("mock")
            .unwrap()
            .with_resources(Resources::new([(CPU, 3.0)]).unwrap()),
    )
    .unwrap();
    assert_eq!(
        record(&room, &made.id())["config"]["resources"],
        serde_json::json!({"cpu": 3.0})
    );
}

/// A new name is for any state -- a running machine is renamed too -- and
/// it is the record's, so it is still there when BeatBox opens again.
#[test]
fn test_a_new_name_is_written_down_and_comes_back() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.start().unwrap();
    made.rename("  bob  ");
    assert_eq!(made.name(), "bob");
    assert_eq!(made.lifecycle(), Lifecycle::Running);
    let written = record(&room, &made.id());
    assert_eq!(written["identity"]["name"], "bob");
    assert_eq!(written["config"]["name"], "bob");
    let sandbox_id = made.id();
    drop(api);
    assert_eq!(
        open(room.path()).runtime().get(&sandbox_id).unwrap().name(),
        "bob"
    );
}

/// Nothing is what creation did with nothing: the type's name.
#[test]
fn test_an_empty_name_is_the_types() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.rename("   ");
    assert_eq!(made.name(), "mock");
}

#[test]
fn test_destroying_one_removes_the_record() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.destroy().unwrap();
    assert!(!room.join(&format!("state/{}.json", made.id())).exists());
}

/// The whole bug: close and open again must show the same sandbox, not an
/// empty list.
#[test]
fn test_it_comes_back_after_the_runtime_is_forgotten() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.start().unwrap();
    let sandbox_id = made.id();
    drop(api);

    let api = open(room.path());
    let restored = api.runtime().get(&sandbox_id).unwrap();
    assert_eq!(restored.name(), "alice");
    assert_eq!(restored.lifecycle(), Lifecycle::Running);
}

#[test]
fn test_a_destroyed_one_does_not_come_back() {
    let room = Room::new("persist");
    let api = open(room.path());
    api.runtime().create(alice()).unwrap().destroy().unwrap();
    drop(api);

    assert!(open(room.path()).runtime().sandboxes().is_empty());
}

/// A build where a provider is missing for a while must not read that as
/// "this sandbox no longer exists" and delete the record out from under a
/// version that registers the type again.
#[test]
fn test_a_record_for_a_type_no_longer_registered_is_skipped_not_lost() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.start().unwrap();
    let path = room.join(&format!("state/{}.json", made.id()));
    drop(api);

    assert!(without_types(&room).runtime().sandboxes().is_empty());
    assert!(
        path.exists(),
        "the record must survive an unregistered type"
    );
}

/// Registering a type late must not lose what was on disk for it.
#[test]
fn test_a_type_registered_after_the_runtime_still_gets_its_sandboxes_back() {
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.start().unwrap();
    drop(api);

    let api = without_types(&room);
    assert!(api.runtime().sandboxes().is_empty());
    api.register(beatbox_engine::MockSandbox::spec()).unwrap();
    assert_eq!(api.runtime().get(&made.id()).unwrap().name(), "alice");
}

#[test]
fn test_a_record_that_is_not_json_is_removed_rather_than_crash_every_launch() {
    capture_logs();
    let room = Room::new("persist");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    let path = room.join(&format!("state/{}.json", made.id()));
    drop(api);
    fs::write(&path, "{not json").unwrap();

    open(room.path());
    // By its name: the logged path is spelt however read_dir spells it.
    assert!(logged(log::Level::Error, &format!("{}.json", made.id())));
    assert!(!path.exists());
}

/// A room is protected by still belonging to a known sandbox, not by a QEMU
/// holding its socket: a sandbox restored as stopped has no process at all.
#[test]
fn test_a_claimed_room_survives_the_sweep_even_stopped() {
    let room = Room::new("persist");
    // Opened first: opening is itself a sweep.
    let api = open(room.path());
    let machine = room.join("machines/sbx-claimed");
    fs::create_dir_all(&machine).unwrap();
    fs::write(machine.join("disk.qcow2"), b"a guest's own disk").unwrap();
    let made = api
        .runtime()
        .create(alice().with_settings(data!({"workspace": machine.to_string_lossy()})))
        .unwrap();
    made.start().unwrap();
    made.stop().unwrap();
    drop(api);

    open(room.path()); // restores "alice" and sweeps, in that order
    assert!(machine.exists());
}

#[test]
fn test_an_unclaimed_room_does_not_survive_the_sweep() {
    let room = Room::new("persist");
    let machine = room.join("machines/sbx-orphan");
    fs::create_dir_all(&machine).unwrap();
    fs::write(machine.join("disk.qcow2"), b"nobody's disk any more").unwrap();

    open(room.path());
    assert!(!machine.exists());
}

// --- only one BeatBox drives the machines ----------------------------------------
//
// With four open, one read a running machine as lost (QEMU's monitor takes
// one client) and overwrote the real record.

struct Owned {
    room: Room,
    other: Option<fs::File>,
    record: std::path::PathBuf,
    before: String,
}

fn somebody_else_is_open() -> Owned {
    let room = Room::new("owner");
    let api = open(room.path());
    let made = api.runtime().create(alice()).unwrap();
    made.start().unwrap();
    drop(api); // "closed", and the lock with it

    let other = fs::OpenOptions::new()
        .create(true)
        .append(true)
        // Windows' LockFileEx wants read or write access; append is neither.
        .read(true)
        .open(room.join("state/.owner"))
        .unwrap();
    assert!(beatbox_isolation::host_os::lock_exclusively(&other));
    let record = room.join(&format!("state/{}.json", made.id()));
    let before = fs::read_to_string(&record).unwrap();
    Owned {
        room,
        other: Some(other),
        record,
        before,
    }
}

#[test]
fn test_a_second_one_cannot_claim() {
    let owned = somebody_else_is_open();
    let api = open(owned.room.path());
    assert!(!api.claim());
    assert!(!api.owns());
}

#[test]
fn test_a_second_one_restores_nothing_and_rewrites_nothing() {
    capture_logs();
    let owned = somebody_else_is_open();
    let api = open(owned.room.path());
    assert!(logged(
        log::Level::Warn,
        &owned.room.join("state").display().to_string()
    ));
    assert!(api.runtime().sandboxes().is_empty());
    let made = api
        .runtime()
        .create(SandboxConfig::new("mock").unwrap().named("bob"))
        .unwrap();
    made.start().unwrap();
    assert_eq!(
        fs::read_to_string(&owned.record).unwrap(),
        owned.before,
        "the owner's record is untouched"
    );
    assert!(
        !owned
            .room
            .join(&format!("state/{}.json", made.id()))
            .exists()
    );
}

#[test]
fn test_once_the_owner_is_gone_the_next_one_claims() {
    let mut owned = somebody_else_is_open();
    let api = open(owned.room.path());
    assert!(!api.owns());
    owned.other.take(); // the kernel lets go when the owner exits
    assert!(api.claim());
}
