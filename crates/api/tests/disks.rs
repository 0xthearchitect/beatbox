//! Making a disk: the one operation that keeps what a guest writes.
//!
//! The guard worth testing hardest: an installer is never pointed at a disk
//! that already exists.
//!
//! Nothing spawns QEMU: `qemu-img` and the installer are fakes in `support`.
//! The fake makes the file; `golden/qemu/qemu-img.json` holds the argv.

mod support;

use std::fs;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use beatbox_api::disks::{KIND, WORKING, windows_answers};
use beatbox_api::jobs::DONE;
use beatbox_api::{Api, ApiParts};
use beatbox_engine::{SandboxConfig, data};
use beatbox_isolation::machine::{
    DiskBuild, Display, UNKNOWN_DISPLAY_PACKAGES, build_install_command, display_available,
    display_install_hint_for, windows_installer_language,
};
use beatbox_isolation::runner::{Output, Runner};
use serde_json::json;
use support::{
    Ran, Room, Stayed, call, code, data, eventually, fake_disc, installer, job, parts, qemu,
    reason_code, settle,
};

// --- whether a window can be shown at all -------------------------------------------

/// A fake `-display help`, answering with whichever backends it is told.
struct Answered(Option<String>);

impl Runner for Answered {
    fn run(&self, _argv: &[String], _timeout: Option<Duration>) -> io::Result<Output> {
        match &self.0 {
            Some(stdout) => Ok(Output {
                code: 0,
                stdout: stdout.clone(),
                stderr: String::new(),
            }),
            None => Err(io::Error::other("no such file")),
        }
    }
}

fn help(names: &[&str]) -> Answered {
    Answered(Some(format!(
        "Available display backend types:\n{}\n\nSome display backends support suboptions...\n",
        names.join("\n")
    )))
}

#[test]
fn test_a_build_with_only_none_has_nothing_to_show() {
    assert!(!display_available(&qemu(), &help(&["none"])));
}

#[test]
fn test_a_build_with_a_real_backend_can_show_something() {
    assert!(display_available(&qemu(), &help(&["gtk", "sdl", "none"])));
}

#[test]
fn test_a_binary_that_will_not_even_answer_cannot_show_anything_either() {
    assert!(!display_available(&qemu(), &Answered(None)));
}

// --- names ----------------------------------------------------------------------------

struct Bench {
    room: Room,
    disc: String,
    api: Api,
}

fn bench_with(display: bool, launch: Arc<dyn beatbox_api::Launch>) -> Bench {
    let room = Room::new("disks");
    let disc = fake_disc(&room.join("installer.iso"));
    let api = Api::open(ApiParts {
        installer: installer(launch, display),
        ..parts(room.path())
    });
    Bench { room, disc, api }
}

fn bench() -> Bench {
    bench_with(true, Arc::new(Ran::default()))
}

fn build(bench: &Bench, source: &str, name: &str, size: i64) -> beatbox_contract::Envelope {
    call(
        &bench.api,
        "disks.build",
        json!({"source": source, "name": name, "sizeGb": size, "network": false, "unattended": false, "machine": null}),
    )
}

/// It becomes a filename in a folder BeatBox owns. A separator in it is a
/// caller writing somewhere else.
#[test]
fn test_a_disk_name_is_a_name_and_not_a_path() {
    let bench = bench();
    for bad in ["../evil", "a/b", "", "  ", ".hidden"] {
        assert_eq!(
            code(&build(&bench, &bench.disc, bad, 20)),
            "bad-name",
            "{bad}"
        );
    }
}

#[test]
fn test_a_disk_that_is_already_there_is_never_written_over() {
    let bench = bench();
    fs::create_dir_all(bench.room.join("disks")).unwrap();
    fs::write(
        bench.room.join("disks/windows.qcow2"),
        b"somebody's machine",
    )
    .unwrap();
    assert_eq!(
        code(&build(&bench, &bench.disc, "windows", 20)),
        "disk-exists"
    );
    assert_eq!(
        fs::read(bench.room.join("disks/windows.qcow2")).unwrap(),
        b"somebody's machine"
    );
}

/// Otherwise QEMU boots under real KVM and sits forever at a screen nobody
/// can see, which is worse than failing.
#[test]
fn test_a_qemu_with_nowhere_to_show_the_installer_is_refused_up_front() {
    let bench = bench_with(false, Arc::new(Ran::default()));
    let refused = build(&bench, &bench.disc, "windows", 20);
    assert_eq!(code(&refused), "bad-disk");
    assert_eq!(reason_code(&refused), "build.no_display");
}

/// Installing from a disk is not an install, it is a copy.
#[test]
fn test_a_source_that_is_not_a_disc_is_refused() {
    let bench = bench();
    let plain = bench.room.join("not-a-disc.qcow2");
    fs::write(&plain, vec![0u8; 2048]).unwrap();
    let refused = build(&bench, &plain.to_string_lossy(), "windows", 20);
    assert_eq!(code(&refused), "bad-disk");
    assert_eq!(reason_code(&refused), "build.source_not_a_disc");
}

#[test]
fn test_a_disc_that_is_not_there_names_the_path() {
    let bench = bench();
    let refused = build(&bench, "/no/such/installer.iso", "windows", 20);
    assert_eq!(reason_code(&refused), "machine.no_such_image");
}

// --- the argv: the safety of an install is almost entirely this list -------------------

fn plan(room: &Room) -> DiskBuild {
    DiskBuild {
        source: fake_disc(&room.join("win.iso")),
        target: room.text("win.qcow2"),
        size_gb: 60,
        ..DiskBuild::default()
    }
}

fn shown() -> Display {
    Display {
        available: true,
        install_hint: String::new(),
    }
}

fn argv(plan: &DiskBuild) -> Vec<String> {
    build_install_command(plan, &qemu(), &shown(), &beatbox_isolation::uuid4()).unwrap()
}

fn after(command: &[String], flag: &str) -> String {
    let at = command.iter().position(|arg| arg == flag).unwrap();
    command[at + 1].clone()
}

#[test]
fn test_it_boots_the_disc_and_writes_to_the_disk() {
    let room = Room::new("argv");
    let plan = plan(&room);
    let command = argv(&plan);
    assert_eq!(after(&command, "-cdrom"), plan.source);
    assert_eq!(after(&command, "-boot"), "once=d");
    assert!(
        command
            .join(" ")
            .contains(&format!("file={},if=none,id=sysdisk", plan.target))
    );
}

/// With `if=virtio`, a stock Windows ISO's "Where do you want to install
/// Windows?" shows no drive at all. AHCI needs no driver disc.
#[test]
fn test_the_destination_disk_is_ahci_not_virtio() {
    let room = Room::new("argv");
    let command = argv(&plan(&room)).join(" ");
    assert!(command.contains("ahci"));
    for virtio in ["if=virtio", "virtio-blk", "virtio-scsi"] {
        assert!(!command.contains(virtio), "{virtio}");
    }
}

/// Everywhere else in BeatBox the writes are thrown away. Here they are the
/// whole point.
#[test]
fn test_it_is_the_one_run_with_no_snapshot() {
    let room = Room::new("argv");
    assert!(!argv(&plan(&room)).contains(&"-snapshot".to_owned()));
}

/// With a plain `-boot d` every reboot goes back to the disc, and a Windows
/// ISO answers that with "Press any key to boot from CD or DVD".
#[test]
fn test_the_disc_boots_once_and_the_disk_every_time_after() {
    let room = Room::new("argv");
    assert_eq!(after(&argv(&plan(&room)), "-boot"), "once=d");
}

#[test]
fn test_an_installer_gets_no_network_unless_it_is_asked_for() {
    let room = Room::new("argv");
    assert_eq!(after(&argv(&plan(&room)), "-nic"), "none");
}

/// The advice this dialog gives -- put the guest agent on this disk --
/// cannot be followed on a machine with no way to fetch it.
#[test]
fn test_a_network_can_be_asked_for() {
    let room = Room::new("argv");
    let asked = DiskBuild {
        network: true,
        ..plan(&room)
    };
    assert_eq!(after(&argv(&asked), "-nic"), "user");
}

/// QEMU's default SMBIOS UUID is the same all-zero value on every
/// unconfigured QEMU, and Windows Autopilot enrols a device by it.
#[test]
fn test_every_install_gets_its_own_uuid_never_the_qemu_default() {
    let room = Room::new("argv");
    let plan = plan(&room);
    let given = after(&argv(&plan), "-uuid");
    assert_ne!(given, "00000000-0000-0000-0000-000000000000");
    assert_ne!(
        after(&argv(&plan), "-uuid"),
        given,
        "a second plan got the same identity as the first"
    );
}

/// The one signal there is: if the agent ends up on the disk, the moment the
/// installed system boots and answers is knowable.
#[test]
fn test_an_install_can_be_heard_from_inside() {
    let room = Room::new("argv");
    let heard = DiskBuild {
        agent_socket: "/tmp/a.sock".into(),
        ..plan(&room)
    };
    assert!(argv(&heard).join(" ").contains("org.qemu.guest_agent.0"));
}

/// On the default machine the optical drive is IDE, and a Windows ISO dies
/// on it without a word.
#[test]
fn test_a_disc_gets_the_chipset_that_can_boot_one() {
    let room = Room::new("argv");
    assert_eq!(after(&argv(&plan(&room)), "-machine"), "q35");
}

#[test]
fn test_nothing_is_shared_with_this_computer() {
    let room = Room::new("argv");
    let command = argv(&plan(&room)).join(" ");
    for hole in ["-virtfs", "-fsdev", "usb-host"] {
        assert!(!command.contains(hole), "{hole}");
    }
}

// --- the job ------------------------------------------------------------------------------

#[test]
fn test_a_build_makes_the_disk_and_boots_the_installer_at_it() {
    let ran = Arc::new(Ran::default());
    let bench = bench_with(true, ran.clone());
    let started = job(build(&bench, &bench.disc, "windows", 1));
    assert_eq!(started.kind, KIND);
    let finished = settle(&bench.api, &started.id, Duration::from_secs(6));
    assert_eq!(finished.state, DONE, "{}", finished.message);
    assert!(bench.room.join("disks/windows.qcow2").is_file());
    let commands = ran.commands.lock().unwrap();
    assert_eq!(commands.len(), 1, "the installer was booted once");
    assert!(commands[0].contains(&"-cdrom".to_owned()));
}

/// Windows Setup reboots into what it installed, so the process never
/// exits; waiting for it left "the installer is up" over a finished desktop.
#[test]
fn test_a_guest_that_does_not_end_itself_keeps_the_job_running() {
    let guest = Stayed::default();
    let bench = bench_with(true, Arc::new(Ran::with(guest.clone())));
    let started = job(build(&bench, &bench.disc, "windows", 1));
    assert!(eventually(Duration::from_secs(2), || {
        bench.api.jobs().get(&started.id).unwrap().message == WORKING
    }));
    let now = bench.api.jobs().get(&started.id).unwrap();
    assert_eq!(now.state, beatbox_api::jobs::RUNNING);
    assert_eq!(now.message, WORKING, "it is waiting on a person");
    assert!(bench.api.building());

    guest.end(); // as if they shut it down from inside
    assert_eq!(
        settle(&bench.api, &started.id, Duration::from_secs(6)).state,
        DONE
    );
}

#[test]
fn test_saying_it_is_installed_shuts_the_guest_down_and_ends_the_job() {
    let guest = Stayed::default();
    let bench = bench_with(true, Arc::new(Ran::with(guest.clone())));
    let started = job(build(&bench, &bench.disc, "windows", 1));
    assert!(eventually(Duration::from_secs(2), || bench.api.building()));

    data(call(&bench.api, "disks.finish", json!({})));
    // Long enough for the monitor to be tried and given up on: on Windows a
    // refused connection to 127.0.0.1 takes about two seconds a try.
    let finished = settle(&bench.api, &started.id, Duration::from_secs(20));
    assert_eq!(finished.state, DONE);
    assert!(
        guest.terminated.load(std::sync::atomic::Ordering::SeqCst),
        "no monitor answered, so it was stopped outright"
    );
    assert_eq!(
        finished.message,
        bench
            .room
            .join("disks")
            .join("windows.qcow2")
            .to_string_lossy()
    );
    assert!(!bench.api.building());
}

#[test]
fn test_nothing_to_finish_when_nothing_is_installing() {
    let bench = bench();
    assert_eq!(
        code(&call(&bench.api, "disks.finish", json!({}))),
        "no-install"
    );
}

#[test]
fn test_a_disk_that_was_made_is_listed() {
    let bench = bench();
    fs::create_dir_all(bench.room.join("disks")).unwrap();
    fs::write(bench.room.join("disks/windows.qcow2"), vec![b'x'; 4096]).unwrap();
    fs::write(bench.room.join("disks/notes.txt"), "not a disk").unwrap();
    let listed = bench.api.disks();
    let names: Vec<&str> = listed.iter().map(|disk| disk.name.as_str()).collect();
    assert_eq!(names, ["windows"]);
    assert_eq!(listed[0].bytes, 4096);
}

#[test]
fn a_beatdrop_raw_base_joins_the_disk_library_and_can_be_forgotten() {
    let bench = bench();
    fs::create_dir_all(bench.room.join("disks")).unwrap();
    let raw = bench.room.join("disks").join("linux.raw");
    fs::write(&raw, vec![0x55; 4096]).unwrap();
    let listed = bench.api.disks();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "linux");
    assert_eq!(listed[0].path, raw.to_string_lossy());
    assert_eq!(
        data(call(&bench.api, "disks.forget", json!({"name": "linux"}))),
        json!([])
    );
    assert!(!raw.exists());
}

#[test]
fn test_a_folder_that_does_not_exist_yet_lists_nothing() {
    let bench = bench();
    assert_eq!(data(call(&bench.api, "disks.list", json!({}))), json!([]));
}

// --- forgetting one -------------------------------------------------------------------------

fn with_a_disk() -> Bench {
    let bench = bench();
    fs::create_dir_all(bench.room.join("disks")).unwrap();
    fs::write(bench.room.join("disks/windows.qcow2"), vec![b'x'; 4096]).unwrap();
    bench
}

fn forget(bench: &Bench, name: &str) -> beatbox_contract::Envelope {
    call(&bench.api, "disks.forget", json!({"name": name}))
}

#[test]
fn test_a_disk_can_be_deleted_and_what_is_left_comes_back() {
    let bench = with_a_disk();
    fs::write(bench.room.join("disks/other.qcow2"), vec![b'y'; 2048]).unwrap();
    let left = data(forget(&bench, "windows"));
    assert!(!bench.room.join("disks/windows.qcow2").exists());
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["name"], "other");
}

/// The one place in BeatBox where mixing a name up with a path removes
/// somebody's work.
#[test]
fn test_a_name_that_is_a_path_deletes_nothing() {
    let bench = with_a_disk();
    let outside = bench.room.join("precious.qcow2");
    fs::write(&outside, b"somebody's machine").unwrap();
    for bad in ["../precious", "/etc/passwd", "windows/../../precious"] {
        assert_eq!(code(&forget(&bench, bad)), "bad-name", "{bad}");
    }
    assert!(outside.exists());
    assert!(bench.room.join("disks/windows.qcow2").exists());
}

/// A disk linked in from another drive: deleting it from BeatBox removes
/// the link, never the file on that drive.
#[cfg(unix)]
#[test]
fn test_a_linked_disk_goes_as_a_link() {
    let bench = with_a_disk();
    let elsewhere = bench.room.join("usb-drive.qcow2");
    fs::write(&elsewhere, vec![b'z'; 4096]).unwrap();
    let link = bench.room.join("disks/usb.qcow2");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    assert!(forget(&bench, "usb").ok);
    assert!(fs::symlink_metadata(&link).is_err());
    assert!(elsewhere.exists());
}

#[test]
fn test_a_disk_that_is_not_there_says_so() {
    let bench = with_a_disk();
    assert_eq!(reason_code(&forget(&bench, "nothing")), "disk.unknown");
}

/// The machine had one spelling of the path, forget resolved another; as
/// text they differed and a running machine's disk was deleted (Windows).
#[cfg(unix)]
#[test]
fn test_a_disk_is_in_use_under_any_name_for_the_same_file() {
    let bench = with_a_disk();
    let alias = bench.room.join("alias");
    std::os::unix::fs::symlink(bench.room.join("disks"), &alias).unwrap();
    bench
        .api
        .runtime()
        .create(
            SandboxConfig::new("mock")
                .unwrap()
                .named("target")
                .with_settings(data!({"disk": alias.join("windows.qcow2").to_string_lossy()})),
        )
        .unwrap();
    assert_eq!(reason_code(&forget(&bench, "windows")), "disk.in_use");
    assert!(bench.room.join("disks/windows.qcow2").exists());
}

/// Deleting it doesn't stop the machine, it fails every uncached read: a
/// corruption nobody would tie to tidying up a disk.
#[test]
fn test_a_disk_a_machine_is_still_using_is_refused() {
    let bench = with_a_disk();
    bench
        .api
        .runtime()
        .create(
            SandboxConfig::new("mock")
                .unwrap()
                .named("target")
                .with_settings(data!({"disk": bench.room.text("disks/windows.qcow2")})),
        )
        .unwrap();
    let refused = forget(&bench, "windows");
    assert_eq!(code(&refused), "disk-in-use");
    assert_eq!(
        refused.error.unwrap().reason.unwrap().values["machine"],
        "target"
    );
    assert!(bench.room.join("disks/windows.qcow2").exists());
}

// --- across distributions (Debian 12, Ubuntu 22.04/24.04, Fedora 44, Tumbleweed) -------

/// What Debian and Ubuntu's qemu-system-x86 lists without qemu-system-gui:
/// not only `none`, so "anything but none" said yes.
#[test]
fn test_a_base_debian_qemu_with_only_headless_backends_shows_nothing() {
    assert!(!display_available(
        &qemu(),
        &help(&["none", "curses", "egl-headless", "dbus"])
    ));
    assert!(!display_available(
        &qemu(),
        &help(&["none", "egl-headless", "curses", "spice-app", "dbus"])
    ));
}

#[test]
fn test_the_hint_names_the_package_of_the_system_it_runs_on() {
    let room = Room::new("osrelease");
    let cases = [
        (
            "ID=ubuntu\nID_LIKE=debian\n",
            "sudo apt install qemu-system-gui",
        ),
        ("ID=debian\n", "sudo apt install qemu-system-gui"),
        (
            "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n",
            "sudo apt install qemu-system-gui",
        ),
        ("ID=fedora\n", "sudo dnf install qemu-ui-gtk"),
        (
            "ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n",
            "sudo zypper install qemu-ui-gtk",
        ),
        ("ID=arch\n", "sudo pacman -S qemu-ui-gtk"),
        (
            "ID=endeavouros\nID_LIKE=arch\n",
            "sudo pacman -S qemu-ui-gtk",
        ),
        ("ID=nixos\n", UNKNOWN_DISPLAY_PACKAGES),
    ];
    for (index, (text, expected)) in cases.iter().enumerate() {
        let path = room.join(&format!("os-release-{index}"));
        fs::write(&path, text).unwrap();
        assert_eq!(display_install_hint_for(&path, false), *expected, "{text}");
    }
    assert_eq!(
        display_install_hint_for(&room.join("missing"), false),
        UNKNOWN_DISPLAY_PACKAGES
    );
}

/// QEMU's Windows installer carries gtk and sdl; one without them came from
/// somewhere else.
#[test]
fn test_on_windows_the_hint_is_where_to_get_qemu() {
    let room = Room::new("osrelease");
    assert!(display_install_hint_for(&room.join("missing"), true).contains("qemu.weilnetz.de"));
}

/// The guest powering off ends an install -- and ends a live session nobody
/// installed from just the same. What is on the disk decides which it was.
mod what_an_install_left {
    use super::*;
    use beatbox_api::disks::installed;
    use beatbox_api::jobs::Said;
    use beatbox_isolation::machine::Qemu;
    use beatbox_isolation::runner::SystemRunner;

    fn disk(room: &Room, name: &str, first: &[u8]) -> String {
        let raw = room.join(&format!("{name}.raw"));
        let mut bytes = first.to_vec();
        bytes.resize(1 << 20, 0);
        std::fs::write(&raw, &bytes).unwrap();
        let target = room.join(&format!("{name}.qcow2"));
        let made = std::process::Command::new("qemu-img")
            .args(["convert", "-q", "-f", "raw", "-O", "qcow2"])
            .arg(&raw)
            .arg(&target)
            .status()
            .unwrap();
        assert!(made.success());
        target.to_string_lossy().into_owned()
    }

    fn code(said: Said) -> String {
        match said {
            Said::Reason(why) => why.code,
            Said::Text(text) => text,
        }
    }

    #[test]
    fn nothing_on_it_is_no_install_and_the_empty_disk_goes() {
        let qemu = Qemu::here();
        if !qemu.image_found {
            return;
        }
        let room = Room::new("installed");
        let target = disk(&room, "blank", &[]);
        let (ok, said) = installed(&target, &qemu, &SystemRunner, &room.join("head"));
        assert!(!ok);
        assert_eq!(code(said), "disks.nothing_installed");
        assert!(!std::path::Path::new(&target).exists());
    }

    #[test]
    fn something_nothing_can_start_from_is_unfinished_and_kept() {
        let qemu = Qemu::here();
        if !qemu.image_found {
            return;
        }
        let room = Room::new("installed");
        let target = disk(&room, "half", &[1, 2, 3]);
        let (ok, said) = installed(&target, &qemu, &SystemRunner, &room.join("head"));
        assert!(!ok);
        assert_eq!(code(said), "disks.install_unfinished");
        assert!(std::path::Path::new(&target).exists());
    }

    #[test]
    fn a_disk_that_can_start_is_an_install() {
        let qemu = Qemu::here();
        if !qemu.image_found {
            return;
        }
        let room = Room::new("installed");
        let mut mbr = vec![0u8; 512];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        let target = disk(&room, "done", &mbr);
        let (ok, said) = installed(&target, &qemu, &SystemRunner, &room.join("head"));
        assert!(ok);
        assert_eq!(code(said), target);
    }
}

// --- an install for a machine being made ---------------------------------------------

/// A disc whose volume label is `label`, where ISO 9660 keeps it.
fn labelled_disc(room: &Room, name: &str, label: &str) -> String {
    let path = room.join(name);
    fake_disc(&path);
    let mut bytes = fs::read(&path).unwrap();
    bytes.resize(16 * 2048 + 40 + 32, 0);
    let mut padded = [b' '; 32];
    padded[..label.len()].copy_from_slice(label.as_bytes());
    bytes[16 * 2048 + 40..16 * 2048 + 72].copy_from_slice(&padded);
    fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn test_a_windows_disc_is_known_by_its_label_and_says_its_language() {
    let room = Room::new("labels");
    for (label, language) in [
        ("CCCOMA_X64FRE_PT-PT_DV9", Some("pt-PT")),
        ("CPBA_X64FRE_EN-US_DV5", Some("en-US")),
        ("CCCOMA_X64FRE_SR-LATN-RS_DV9", Some("sr-Latn-RS")),
        // The Media Creation Tool's: no language to name, so not unwatched.
        ("ESD-ISO", None),
        // A server takes other keys.
        ("SSS_X64FREE_EN-US_DV9", None),
        ("CCCOMA_A64FRE_EN-US_DV9", None),
        ("Ubuntu 24.04.1 LTS amd64", None),
    ] {
        let disc = labelled_disc(&room, &format!("{label}.iso"), label);
        assert_eq!(
            windows_installer_language(&disc).as_deref(),
            language,
            "{label}"
        );
    }
    let plain = room.join("disk.qcow2");
    fs::write(&plain, vec![0u8; 40_000]).unwrap();
    assert_eq!(windows_installer_language(&plain.to_string_lossy()), None);
}

fn build_for_a_machine(bench: &Bench, source: &str) -> beatbox_contract::Envelope {
    call(
        &bench.api,
        "disks.build",
        json!({"source": source, "name": "made", "sizeGb": 1, "network": false, "unattended": true, "machine": null}),
    )
}

/// Answered, so nobody sees it: no window needed, and none opened.
#[test]
fn test_a_windows_disc_for_a_machine_installs_answered_and_without_a_window() {
    let ran = Arc::new(Ran::default());
    let bench = bench_with(false, ran.clone());
    let disc = labelled_disc(&bench.room, "win11.iso", "CCCOMA_X64FRE_PT-PT_DV9");
    let started = job(build_for_a_machine(&bench, &disc));
    let finished = settle(&bench.api, &started.id, Duration::from_secs(6));
    assert_eq!(finished.state, DONE, "{}", finished.message);
    let commands = ran.commands.lock().unwrap();
    let argv = &commands[0];
    // It ends by switching itself off once sysprep has generalized the disk:
    // Windows 11 reboots halfway through copying, so not at a reboot.
    assert!(!argv.contains(&"-no-reboot".to_owned()));
    assert!(argv.windows(2).any(|pair| pair == ["-display", "none"]));
    assert!(
        argv.iter()
            .any(|arg| arg.starts_with("driver=vvfat,node-name=answers,"))
    );
    let answers = windows_answers("pt-PT");
    assert!(answers.contains("<UILanguage>pt-PT</UILanguage>"));
    assert!(answers.contains("<Mode>Audit</Mode>"));
    assert!(
        answers.contains(
            r"/generalize /oobe /shutdown /quiet /unattend:C:\Windows\Panther\machine.xml"
        )
    );
    assert!(!answers.contains("LANGUAGE") && !answers.contains("MACHINE"));
}

/// Any other disc is clicked through, so it still needs its window, and
/// still ends at its first reboot.
#[test]
fn test_another_disc_for_a_machine_opens_its_window_and_ends_at_its_reboot() {
    let refused = build_for_a_machine(&bench_with(false, Arc::new(Ran::default())), &bench().disc);
    assert_eq!(reason_code(&refused), "build.no_display");

    let ran = Arc::new(Ran::default());
    let bench = bench_with(true, ran.clone());
    let started = job(build_for_a_machine(&bench, &bench.disc));
    settle(&bench.api, &started.id, Duration::from_secs(6));
    let argv = &ran.commands.lock().unwrap()[0];
    assert!(argv.contains(&"-no-reboot".to_owned()));
    assert!(!argv.iter().any(|arg| arg.contains("vvfat")));
    assert!(!argv.windows(2).any(|pair| pair == ["-display", "none"]));
}

/// The machine is made and started by the job, from the disk it just made,
/// whether or not anything is still listening: the job answers with its id.
#[test]
fn test_an_install_for_a_machine_ends_with_that_machine_made_and_started() {
    let ran = Arc::new(Ran::default());
    let bench = bench_with(true, ran.clone());
    let started = job(call(
        &bench.api,
        "disks.build",
        json!({"source": bench.disc, "name": "made", "sizeGb": 1, "network": false,
               "unattended": true,
               "machine": {"type": "mock", "name": "lab", "cpu": 1, "memory": 2048,
                           "settings": {"network": "none"}, "uses": {}}}),
    ));
    let finished = settle(&bench.api, &started.id, Duration::from_secs(6));
    assert_eq!(finished.state, DONE, "{}", finished.message);
    let view = data(call(
        &bench.api,
        "sandbox.view",
        json!({"id": finished.message}),
    ));
    assert_eq!(view["name"], "lab");
    assert_eq!(view["state"]["lifecycle"], "running");
    assert_eq!(
        view["config"]["settings"]["disk"],
        bench
            .room
            .join("disks")
            .join("made.qcow2")
            .to_string_lossy()
            .as_ref()
    );
    // Installed with the memory the machine asked for, not 4 GB beside it.
    let argv = &ran.commands.lock().unwrap()[0];
    assert!(
        argv.windows(2).any(|pair| pair == ["-m", "2048"]),
        "{argv:?}"
    );
}

/// A disc is something to install from, and boots without a master boot
/// record: a Windows disc was called unbootable, and could not be chosen.
#[test]
fn test_a_disc_is_inspected_as_something_to_install_from() {
    let bench = bench();
    let windows = labelled_disc(&bench.room, "win.iso", "CCCOMA_X64FRE_EN-US_DV9");
    let seen = data(call(&bench.api, "disks.inspect", json!({"path": windows})));
    assert_eq!(seen["installer"], "windows");
    assert_eq!(seen["bootable"], true);
    let seen = data(call(
        &bench.api,
        "disks.inspect",
        json!({"path": bench.disc}),
    ));
    assert_eq!(seen["installer"], "disc");
}

/// A guest killed from outside (OOM, mid Windows Setup) is a failed install
/// even with a boot record on its disk, and no machine is made of it.
#[test]
fn test_an_installer_killed_from_outside_fails_and_makes_no_machine() {
    struct Killed;
    impl beatbox_api::Guest for Killed {
        fn running(&mut self) -> bool {
            false
        }
        fn stop(&mut self, _timeout: Duration) {}
        fn ended_well(&mut self) -> bool {
            false
        }
    }
    struct LaunchKilled;
    impl beatbox_api::Launch for LaunchKilled {
        fn launch(&self, _argv: &[String]) -> io::Result<Box<dyn beatbox_api::Guest>> {
            Ok(Box::new(Killed))
        }
    }
    let bench = bench_with(true, Arc::new(LaunchKilled));
    let started = job(call(
        &bench.api,
        "disks.build",
        json!({"source": bench.disc, "name": "half", "sizeGb": 1, "network": false,
               "unattended": true,
               "machine": {"type": "mock", "name": "lab", "cpu": 1, "memory": 512,
                           "settings": {}, "uses": {}}}),
    ));
    let finished = settle(&bench.api, &started.id, Duration::from_secs(6));
    assert_eq!(finished.state, beatbox_api::jobs::FAILED);
    assert_eq!(finished.reason.unwrap().code, "disks.install_killed");
    assert!(!bench.room.join("disks/half.qcow2").exists());
    assert!(
        data(call(&bench.api, "sandbox.list", json!({})))
            .as_array()
            .unwrap()
            .is_empty()
    );
}
