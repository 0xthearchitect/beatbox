//! The first real sandbox, and the proof that the contract took it unchanged.
//!
//! *Without QEMU:* what a configuration becomes (capabilities, argv,
//! refusals before start). Disks are files with the right bytes where
//! `is_optical` looks.
//!
//! *With QEMU:* a real guest driven through the engine, one at a time,
//! skipped where QEMU is missing. The disk is empty: everything tested is
//! outside the guest, QMP answers before boot, and each walk takes a second.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{
    ActionOutcome, Data, EngineError, Isolation, IsolationPolicy, Lifecycle, Resources, Sandbox,
    SandboxConfig, SandboxRegistry, data,
};
use beatbox_host::{Agent, CAPABILITIES, HostSandbox, actions};
use beatbox_isolation::machine::{build_command, disk_args};
use beatbox_isolation::qmp::{ControlError, Monitor};
use beatbox_reasons::reason;
use serde_json::{Value, json};
use support::{
    Guarded, Room, build, build_with, empty_disk, fake_disc, one_guest_at_a_time, qemu,
    with_a_screen,
};

/// A successful answer, holding what `actions()` promises for it.
fn answered(outcome: &ActionOutcome) {
    assert!(outcome.ok, "{:?}", outcome.reason);
    let spec = actions()
        .into_iter()
        .find(|spec| spec.capability == outcome.capability())
        .unwrap_or_else(|| panic!("{} has no spec", outcome.capability()));
    spec.assert_outputs(&outcome.data);
}

/// Every capability past the lifecycle says what it takes and gives back.
#[test]
fn every_action_says_what_it_takes_and_gives_back() {
    let described: Vec<String> = actions().into_iter().map(|spec| spec.capability).collect();
    for capability in CAPABILITIES {
        if !capability.starts_with("lifecycle.") {
            assert!(
                described.iter().any(|d| d == capability),
                "{capability} has no spec"
            );
        }
    }
    beatbox_engine::SandboxRegistry::default()
        .register(HostSandbox::spec())
        .unwrap();
}

fn host(settings: Data) -> SandboxConfig {
    SandboxConfig::new("host").unwrap().with_settings(settings)
}

fn sized(cpu: f64, memory: f64) -> Resources {
    let mut amounts = Vec::new();
    if cpu > 0.0 {
        amounts.push((CPU, cpu));
    }
    if memory > 0.0 {
        amounts.push((MEMORY, memory));
    }
    Resources::new(amounts).unwrap()
}

fn spec_of(sandbox: &Sandbox) -> beatbox_isolation::machine::MachineSpec {
    sandbox
        .provider(|p: &mut HostSandbox| p.machine_spec())
        .unwrap()
        .unwrap()
}

fn argv(sandbox: &Sandbox) -> String {
    build_command(&spec_of(sandbox), &qemu(), "u")
        .unwrap()
        .join(" ")
}

/// What a host can be asked to do depends on the disk it was given.
mod capability_tests {
    use super::*;

    fn make(room: &Room, disc: &str, extra: Data) -> (Sandbox, Guarded) {
        let (runtime, _) = build_with(with_a_screen());
        let mut settings =
            data!({"disk": disc, "workspace": room.text(&format!("sbx{}", extra.len()))});
        settings.extend(extra);
        let made = runtime.create(host(settings).named("target")).unwrap();
        (made, runtime)
    }

    /// `media.insert` needs the USB controller only a windowed machine has --
    /// and a drive nobody can see to open is not worth the hole.
    #[test]
    fn a_drive_can_only_be_plugged_into_a_machine_with_a_screen() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (hidden, _hidden_runtime) = make(&room, &disc, data!({"x": 1}));
        assert!(!hidden.capabilities().supports("media.insert"));
        let (shown, _shown_runtime) = make(&room, &disc, data!({"display": true}));
        let (hostile, _hostile_runtime) = make(
            &room,
            &disc,
            data!({"display": true, "hostile": true, "y": 1}),
        );
        assert!(shown.capabilities().supports("media.insert"));
        assert!(
            !hostile.capabilities().supports("media.insert"),
            "nothing of this computer's is handed to a hostile guest"
        );
    }

    #[test]
    fn a_host_isolates_at_the_machine_level_and_says_so() {
        let mut registry = SandboxRegistry::default();
        let entry = registry.register(HostSandbox::spec()).unwrap();
        assert_eq!(entry.isolation, Isolation::Machine);
    }

    /// A floor, not an equality. Being handed something stronger than was
    /// asked for is the only surprise that is safe.
    #[test]
    fn a_configuration_asking_for_a_weaker_boundary_still_gets_this_one() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(
                host(data!({"disk": disc, "workspace": room.text("weak")}))
                    .with_isolation(IsolationPolicy::at(Isolation::Container)),
            )
            .unwrap();
        assert_eq!(sandbox.lifecycle(), Lifecycle::Created);
    }

    /// There is nothing to write into: a disc is read-only and gets no
    /// overlay, so every snapshot verb is withheld rather than refused later.
    #[test]
    fn a_disc_cannot_be_snapshotted_and_does_not_claim_it_can() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (sandbox, _sandbox_runtime) = make(&room, &disc, Data::new());
        assert!(
            sandbox
                .capabilities()
                .matching("snapshot.*")
                .unwrap()
                .is_empty()
        );
        assert!(sandbox.capabilities().supports("process.execute"));
    }

    #[test]
    fn a_host_with_no_agent_channel_cannot_be_asked_to_do_anything_inside() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (sandbox, _sandbox_runtime) = make(&room, &disc, data!({"agent": false}));
        assert!(
            sandbox
                .capabilities()
                .matching("process.*")
                .unwrap()
                .is_empty()
        );
        assert!(
            sandbox
                .capabilities()
                .matching("filesystem.*")
                .unwrap()
                .is_empty()
        );
        assert!(
            sandbox.capabilities().supports("lifecycle.pause"),
            "the monitor is still there"
        );
    }

    /// A machine with a screen keeps a picture of it, which the page shows
    /// while it is off: none yet is an empty picture, not a failure; one
    /// kept is read back as it was.
    #[test]
    fn a_machine_with_a_screen_shows_the_picture_it_kept() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (hidden, _hidden_runtime) = make(&room, &disc, data!({"x": 1, "y": 1}));
        assert!(!hidden.capabilities().supports("display.picture"));
        let (shown, _runtime) = make(&room, &disc, data!({"display": true}));
        let none = shown.act("display.picture", data!({})).unwrap();
        assert!(none.ok);
        assert_eq!(none.data["picture"], json!(""));
        assert_eq!(none.data["taken"], json!(0));
        std::fs::write(room.join("sbx1/screen.png"), b"png").unwrap();
        let kept = shown.act("display.picture", data!({})).unwrap();
        assert_eq!(kept.data["picture"], json!("cG5n"));
        assert!(kept.data["taken"].as_u64().unwrap() > 0);
    }

    /// Before this module is touched. That is the whole point of declaring
    /// capabilities rather than failing at the bottom.
    #[test]
    fn the_runtime_refuses_an_action_the_disk_cannot_support() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (sandbox, _sandbox_runtime) = make(&room, &disc, data!({"agent": false}));
        let outcome = sandbox
            .act("process.execute", data!({"command": "/bin/true"}))
            .unwrap();
        assert!(!outcome.ok);
        assert_eq!(outcome.reason.unwrap().code, "engine.unsupported");
    }

    /// A pen holds what FAT16 can, and a folder bigger than that is told so
    /// before QEMU has to refuse it -- nothing is started to find out.
    #[test]
    fn a_pen_too_big_for_a_pen_is_refused_first() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (sandbox, _runtime) = make(&room, &disc, data!({"display": true, "p": 1}));
        assert!(sandbox.capabilities().supports("media.pen"));
        std::fs::create_dir_all(room.join("big")).unwrap();
        std::fs::File::create(room.join("big/sample.bin"))
            .unwrap()
            .set_len(600 * 1024 * 1024)
            .unwrap();
        let refused = sandbox
            .act("media.pen", data!({"folder": room.text("big")}))
            .unwrap();
        assert_eq!(refused.reason.unwrap().code, "host.pen_too_big");
    }

    /// `ps` on Linux, `tasklist` on Windows; a guest described as anything
    /// else has neither, and is not offered a list it cannot have.
    #[test]
    fn process_list_is_offered_where_there_is_ps_or_tasklist() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (windows, _windows_runtime) = make(
            &room,
            &disc,
            data!({"os": {"family": "windows", "edition": "11"}}),
        );
        assert!(windows.capabilities().supports("process.list"));
        assert!(windows.capabilities().supports("process.kill"));
        let (other, _other_runtime) = make(&room, &disc, data!({"os": {"family": "bsd"}, "z": 1}));
        assert!(!other.capabilities().supports("process.list"));
        assert!(!other.capabilities().supports("process.kill"));
        assert!(other.capabilities().supports("process.execute"));
    }
}

mod refusal_tests {
    use super::*;

    fn refused(made: Result<Sandbox, EngineError>) -> beatbox_reasons::Reason {
        match made {
            Err(EngineError::Provider(error)) => error.reason.unwrap(),
            other => panic!("expected a provider's refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_host_with_no_disk_is_refused_before_anything_is_made() {
        let room = Room::new("host");
        let (runtime, _) = build();
        let why = refused(runtime.create(host(data!({"workspace": room.text("a")}))));
        assert_eq!(why.code, "host.no_disk");
        assert!(runtime.every_sandbox().is_empty());
    }

    #[test]
    fn a_disk_that_is_not_there_names_the_path() {
        let room = Room::new("host");
        let (runtime, _) = build();
        let why = refused(runtime.create(host(
            data!({"disk": "/no/such/disk.qcow2", "workspace": room.text("b")}),
        )));
        assert_eq!(why.code, "machine.no_such_image");
        assert_eq!(why.values["path"], "/no/such/disk.qcow2");
    }

    #[test]
    fn a_disconnected_source_is_reported_and_refused_without_resetting_the_disk() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(host(
                data!({"disk": disc, "workspace": room.text("machine")}),
            ))
            .unwrap();
        assert_eq!(sandbox.refresh().unwrap().facts["disk_available"], true);

        let away = room.join("disconnected.iso");
        std::fs::rename(&disc, &away).unwrap();
        assert_eq!(sandbox.refresh().unwrap().facts["disk_available"], false);
        assert_eq!(sandbox.lifecycle(), Lifecycle::Created);
        let marker = room.join("machine/disk.qcow2");
        std::fs::write(&marker, b"saved changes").unwrap();
        let reset = sandbox
            .provider(|p: &mut HostSandbox| beatbox_engine::SandboxProvider::reset(p))
            .unwrap()
            .map_err(EngineError::Provider);
        for refusal in [sandbox.start().map(|_| ()), reset] {
            let why = match refusal {
                Err(EngineError::Provider(error)) => error.reason.unwrap(),
                other => panic!("expected a missing source refusal, got {other:?}"),
            };
            assert_eq!(why.code, "machine.no_such_image");
            assert_eq!(why.values["path"], disc);
        }
        assert_eq!(std::fs::read(&marker).unwrap(), b"saved changes");

        std::fs::rename(away, &disc).unwrap();
        assert_eq!(sandbox.refresh().unwrap().facts["disk_available"], true);
        assert_eq!(sandbox.lifecycle(), Lifecycle::Created);
    }

    #[test]
    fn a_restored_failed_machine_reports_its_missing_source_disk() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(host(
                data!({"disk": disc, "workspace": room.text("machine")}),
            ))
            .unwrap();
        std::fs::rename(&disc, room.join("disconnected.iso")).unwrap();
        let (next_run, _) = build();
        let restored = next_run
            .restore(
                sandbox.identity(),
                sandbox.config(),
                Lifecycle::Failed,
                Some(reason!("machine.channel_gone")),
            )
            .unwrap();
        assert_eq!(restored.state().facts["disk_available"], false);
        assert_eq!(restored.state().facts["disk"], disc);
    }

    #[test]
    fn a_failed_creation_leaves_no_workspace_and_no_allocation() {
        let room = Room::new("host");
        let (runtime, _) = build();
        let made = runtime.create(
            host(data!({"disk": "/no/such/disk.qcow2", "workspace": room.text("c")}))
                .with_resources(sized(4.0, 0.0)),
        );
        assert!(made.is_err());
        assert!(!room.join("c").exists());
        assert!(runtime.resources().committed().is_empty());
    }
}

/// Where generic becomes specific: the one place the runtime's units and the
/// provider's flags meet.
mod spec_tests {
    use super::*;

    struct Made {
        sandbox: Sandbox,
        _runtime: Guarded,
        _room: Room,
    }

    fn make_with(
        environment: beatbox_host::Environment,
        resources: Resources,
        extra: Data,
    ) -> Made {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let mut settings = data!({"disk": disc, "workspace": room.text("s")});
        settings.extend(extra);
        let (runtime, _) = build_with(environment);
        let sandbox = runtime
            .create(host(settings).with_resources(resources))
            .unwrap();
        Made {
            sandbox,
            _runtime: runtime,
            _room: room,
        }
    }

    fn make(resources: Resources, extra: Data) -> Made {
        make_with(beatbox_host::Environment::here(), resources, extra)
    }

    #[test]
    fn the_ledgers_numbers_become_the_hypervisors_flags() {
        let made = make(sized(4.0, 8192.0), Data::new());
        let spec = spec_of(&made.sandbox);
        assert_eq!((spec.cpus, spec.memory_mb), (4, 8192));
        let command = build_command(&spec, &qemu(), "u").unwrap();
        let after =
            |flag: &str| command[command.iter().position(|c| c == flag).unwrap() + 1].clone();
        assert_eq!(after("-smp"), "4");
        assert_eq!(after("-m"), "8192");
    }

    #[test]
    fn a_beatbox_display_replaces_qemus_native_window() {
        let made = make_with(
            with_a_screen(),
            sized(2.0, 2048.0),
            data!({"display": true}),
        );
        let mut spec = spec_of(&made.sandbox);
        spec.display_socket = made._room.text("display.sock");
        spec.display_websocket = "tcp:127.0.0.1:5901".into();
        spec.display_password_file = made._room.text("display.password");
        let command = build_command(&spec, &qemu(), "u").unwrap();
        assert!(command.windows(2).any(|pair| pair == ["-display", "none"]));
        let vnc = command[command.iter().position(|part| part == "-vnc").unwrap() + 1].clone();
        assert!(vnc.contains("websocket=127.0.0.1:5901"));
        assert!(vnc.contains("password-secret=beatbox-display-password"));
        assert!(!vnc.contains("eight888"));
    }

    /// The ledger is fractional because a ledger can be. `-smp 0.5` is not a
    /// thing, and rounding down would be a guest with no processor.
    #[test]
    fn half_a_core_is_a_whole_one_to_a_hypervisor() {
        let made = make(sized(0.5, 0.0), Data::new());
        assert_eq!(spec_of(&made.sandbox).cpus, 1);
    }

    #[test]
    fn a_host_nobody_sized_still_boots() {
        let spec = spec_of(&make(sized(0.0, 0.0), Data::new()).sandbox);
        assert_eq!((spec.cpus, spec.memory_mb), (2, 2048));
    }

    #[test]
    fn no_card_at_all_unless_one_was_asked_for() {
        let made = make(sized(0.0, 0.0), Data::new());
        assert_eq!(spec_of(&made.sandbox).network, "none");
        assert!(argv(&made.sandbox).contains("-nic none"));
    }

    #[test]
    fn both_control_channels_are_opened_and_neither_is_a_shared_folder() {
        let command = argv(&make(sized(0.0, 0.0), Data::new()).sandbox);
        assert!(command.contains("-qmp"));
        assert!(command.contains("org.qemu.guest_agent.0"));
        for hole in ["-virtfs", "-fsdev", "usb-host"] {
            assert!(!command.contains(hole), "{hole}");
        }
    }

    /// Found by a real install showing a real, unrelated company's real
    /// Autopilot branding: QEMU's default SMBIOS UUID is a fixed all-zero
    /// value, and Windows enrolls a device by it without checking it is
    /// unique. Every start draws a fresh one.
    #[test]
    fn a_host_never_wears_qemus_default_hardware_identity() {
        let made = make(sized(0.0, 0.0), Data::new());
        let uuid = (beatbox_host::Environment::here().uuid)();
        let command = build_command(&spec_of(&made.sandbox), &qemu(), &uuid).unwrap();
        let given = &command[command.iter().position(|c| c == "-uuid").unwrap() + 1];
        assert_ne!(given, "00000000-0000-0000-0000-000000000000");
        assert_ne!(
            given,
            &(beatbox_host::Environment::here().uuid)(),
            "and never the same twice"
        );
    }

    /// Found by a real Windows 10 machine said to have "a way out to the
    /// internet" that had no network card at all: Windows carries no
    /// virtio-net driver.
    #[test]
    fn the_card_is_one_every_system_has_a_driver_for() {
        let command = argv(&make(sized(0.0, 0.0), data!({"network": "nat"})).sandbox);
        assert!(command.contains("e1000,netdev=net0"));
        assert!(!command.contains("virtio-net"));
    }

    #[test]
    fn a_screen_on_a_qemu_that_cannot_draw_one_is_refused_by_name() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let mut environment = beatbox_host::Environment::here();
        environment.display_available = Arc::new(|_| false);
        environment.install_hint = Arc::new(|| "sudo pacman -S qemu-ui-gtk".into());
        let (runtime, _) = build_with(environment);
        let made = runtime.create(host(
            data!({"disk": disc, "workspace": room.text("s"), "display": true}),
        ));
        let Err(EngineError::Provider(error)) = made else {
            panic!("a screen QEMU cannot draw was accepted");
        };
        assert_eq!(error.reason.unwrap().code, "build.no_display");
    }

    /// Found by a Windows guest killed by the kernel twice in three minutes:
    /// QEMU boots a guest bigger than what is free, and the kernel takes it
    /// away later with nothing on the screen but "went away".
    #[test]
    fn more_memory_than_the_host_has_free_is_refused_before_booting() {
        let mut environment = beatbox_host::Environment::here();
        environment.available_memory_mb = Arc::new(|| Some(4400));
        let made = make_with(environment, sized(0.0, 4096.0), Data::new());
        let Err(EngineError::Provider(error)) = made.sandbox.start() else {
            panic!("a guest bigger than what is free was started");
        };
        let why = error.reason.unwrap();
        assert_eq!(why.code, "host.not_enough_memory");
        assert_eq!(why.values["wanted"], "4");
        let started = made
            .sandbox
            .provider(|p: &mut HostSandbox| p.machine_mut().is_some())
            .unwrap();
        assert!(!started, "nothing was started");
    }

    /// "Spend as little as possible", as argv. Lose one and a machine
    /// quietly costs more, which nothing else would notice.
    #[test]
    fn a_machine_is_built_to_cost_the_host_as_little_as_it_can() {
        let made = make(sized(0.0, 0.0), Data::new());
        let spec = spec_of(&made.sandbox);
        let kvm = beatbox_isolation::machine::Qemu {
            kvm: true,
            ..qemu()
        };
        let command = build_command(&spec, &kvm, "u").unwrap().join(" ");
        assert!(command.contains("virtio-balloon-pci,free-page-reporting=on"));
        assert!(command.contains("hv-time=on"));
        assert!(command.contains("hv-stimer=on"));
        assert!(spec.log_path.ends_with("qemu.log"));
    }

    /// A USB tablet is absolute, on xHCI because UHCI is polled -- and only
    /// with a screen.
    #[test]
    fn a_machine_with_a_screen_has_a_pointer_that_follows_the_host() {
        let shown =
            argv(&make_with(with_a_screen(), sized(0.0, 0.0), data!({"display": true})).sandbox);
        assert!(shown.contains("usb-tablet"));
        assert!(shown.contains("qemu-xhci"));
        assert!(!shown.replace("qemu-xhci", "").contains("uhci"));
        let hidden = argv(&make(sized(0.0, 0.0), Data::new()).sandbox);
        assert!(!hidden.contains("usb-tablet"));
    }

    /// discard lets the overlay shrink; rotation_rate=1 makes Windows trim
    /// rather than defragment -- which rewrites the whole disk into it.
    #[test]
    fn what_the_guest_deletes_leaves_its_disk() {
        let command = disk_args("/x.qcow2", "sysdisk").join(" ");
        for saving in ["discard=unmap", "detect-zeroes=unmap", "rotation_rate=1"] {
            assert!(command.contains(saving), "{saving}");
        }
    }

    /// Processors, memory and network are read again at the next start. The
    /// disk is what its overlay is built on, so it is refused by name.
    #[test]
    fn a_host_can_be_resized_but_not_moved_to_another_disk() {
        let made = make(sized(0.0, 0.0), Data::new());
        let mut wider = made.sandbox.config().settings;
        wider.insert("network".into(), json!("isolated"));
        made.sandbox
            .reconfigure(host(wider.clone()).with_resources(sized(3.0, 1024.0)))
            .unwrap();
        let spec = spec_of(&made.sandbox);
        assert_eq!(
            (spec.cpus, spec.memory_mb, spec.network.as_str()),
            (3, 1024, "isolated")
        );
        let mut moved = wider;
        moved.insert("disk".into(), json!("/elsewhere.qcow2"));
        let Err(EngineError::Provider(error)) = made.sandbox.reconfigure(host(moved)) else {
            panic!("a host was moved to another disk");
        };
        assert_eq!(error.reason.unwrap().code, "host.fixed_setting");
        assert_eq!(
            made.sandbox.config().setting("network").unwrap(),
            "isolated",
            "nothing changed"
        );
    }

    #[test]
    fn a_host_with_no_agent_opens_no_channel_into_the_guest() {
        let made = make(sized(0.0, 0.0), data!({"agent": false}));
        assert_eq!(spec_of(&made.sandbox).agent_socket, "");
        assert!(!argv(&made.sandbox).contains("org.qemu.guest_agent.0"));
    }

    /// Nothing to keep: there is no overlay, because there is nothing
    /// writable to overlay.
    #[test]
    fn a_disc_still_runs_the_disposable_way() {
        let made = make(sized(0.0, 0.0), Data::new());
        assert!(spec_of(&made.sandbox).disposable);
        assert!(argv(&made.sandbox).contains("-snapshot"));
    }

    #[test]
    fn a_network_nobody_recognises_is_refused_rather_than_ignored() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (runtime, _) = build();
        let made = runtime.create(host(
            data!({"disk": disc, "workspace": room.text("s"), "network": "bridged"}),
        ));
        let Err(EngineError::Provider(error)) = made else {
            panic!("an unknown network was accepted");
        };
        assert_eq!(error.reason.unwrap().code, "machine.bad_network");
    }

    /// The rule the machine tier existed to enforce, and it survived the
    /// tier: it is checked in MachineSpec, which is the host sandbox's.
    #[test]
    fn hostile_work_still_cannot_have_a_route_out() {
        let room = Room::new("host");
        let disc = fake_disc(&room.join("installer.iso"));
        let (runtime, _) = build();
        let made = runtime.create(host(
            data!({"disk": disc, "workspace": room.text("s"), "hostile": true, "network": "nat"}),
        ));
        let Err(EngineError::Provider(error)) = made else {
            panic!("hostile work was given a route out");
        };
        assert_eq!(
            error.reason.unwrap().code,
            "machine.hostile_with_a_route_out"
        );
    }
}

/// A guest that actually starts, and the contract driven against it.
mod real_guest_tests {
    use super::*;

    struct Guest {
        sandbox: Sandbox,
        runtime: Guarded,
        log: beatbox_engine::Recorder,
        room: Room,
    }

    fn guest() -> Guest {
        let room = Room::new("guest");
        let base = empty_disk(&room.join("base.qcow2"));
        let (runtime, log) = build();
        let sandbox = runtime
            .create(
                host(data!({"disk": base, "workspace": room.text("sbx")}))
                    .named("target")
                    .with_resources(sized(1.0, 256.0)),
            )
            .unwrap();
        Guest {
            sandbox,
            runtime,
            log,
            room,
        }
    }

    fn overlay(sandbox: &Sandbox) -> PathBuf {
        PathBuf::from(sandbox.state().facts["overlay"].as_str().unwrap())
    }

    fn status(sandbox: &Sandbox) -> Value {
        sandbox.refresh().unwrap().facts["status"].clone()
    }

    #[test]
    fn a_disk_of_its_own_is_made_and_the_users_image_is_not_touched() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        let overlay = overlay(&g.sandbox);
        assert!(overlay.is_file());
        let info = Command::new("qemu-img")
            .arg("info")
            .arg(&overlay)
            .output()
            .unwrap();
        let info = String::from_utf8_lossy(&info.stdout).into_owned();
        let backing = info
            .lines()
            .find_map(|line| line.strip_prefix("backing file:"))
            .unwrap()
            .trim();
        assert_eq!(
            std::fs::canonicalize(backing).unwrap(),
            std::fs::canonicalize(g.room.join("base.qcow2")).unwrap()
        );
    }

    /// Create -> start -> pause -> resume -> snapshot -> restore -> stop ->
    /// reset -> destroy, against QEMU, through the engine.
    #[test]
    fn the_walk() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        let s = &g.sandbox;
        assert_eq!(s.start().unwrap().lifecycle, Lifecycle::Running);
        assert_eq!(s.state().facts["status"], "running");

        s.pause().unwrap();
        assert_eq!(status(s), "paused");
        s.resume().unwrap();
        assert_eq!(status(s), "running");

        answered(
            &s.act("snapshot.create", data!({"name": "before_attack"}))
                .unwrap(),
        );
        let listed = s.act("snapshot.list", data!({})).unwrap();
        answered(&listed);
        assert_eq!(listed.data["snapshots"], json!(["before_attack"]));
        answered(
            &s.act("snapshot.restore", data!({"name": "before_attack"}))
                .unwrap(),
        );
        assert!(
            !s.act("snapshot.restore", data!({"name": "never_taken"}))
                .unwrap()
                .ok
        );

        assert_eq!(s.stop().unwrap().lifecycle, Lifecycle::Stopped);
        assert_eq!(s.reset().unwrap().lifecycle, Lifecycle::Created);
        assert_eq!(s.destroy().unwrap().lifecycle, Lifecycle::Destroyed);
    }

    #[test]
    fn the_story_the_events_tell() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        let s = &g.sandbox;
        s.start().unwrap();
        s.pause().unwrap();
        s.resume().unwrap();
        s.act("snapshot.create", data!({"name": "clean"})).unwrap();
        s.stop().unwrap();
        let names = g.log.names();
        for expected in [
            "sandbox.created",
            "sandbox.started",
            "sandbox.paused",
            "guest.stop",
            "sandbox.resumed",
            "guest.resume",
            "snapshot.created",
            "sandbox.stopped",
        ] {
            assert!(
                names.contains(&expected.to_owned()),
                "{expected} in {names:?}"
            );
        }
    }

    #[test]
    fn resetting_throws_the_overlay_away_and_makes_a_new_one() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        let s = &g.sandbox;
        s.start().unwrap();
        s.act("snapshot.create", data!({"name": "clean"})).unwrap();
        let overlay = overlay(s);
        let before = std::fs::metadata(&overlay).unwrap().len();
        s.reset().unwrap();
        assert!(overlay.is_file(), "it is a new one, not no one");
        assert!(std::fs::metadata(&overlay).unwrap().len() <= before);
        assert_eq!(
            s.act("snapshot.list", data!({})).unwrap().data["snapshots"],
            json!([]),
            "the snapshots lived in the file that was deleted"
        );
    }

    /// An empty disk has no guest agent, and the refusal says so rather than
    /// timing out or claiming the sandbox is broken.
    #[test]
    fn nothing_inside_can_be_asked_without_something_inside_to_ask() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        let outcome = g
            .sandbox
            .act("process.execute", data!({"command": "/bin/true"}))
            .unwrap();
        assert!(!outcome.ok);
        assert_eq!(outcome.reason.unwrap().code, "host.no_agent");
        assert_eq!(
            g.sandbox.lifecycle(),
            Lifecycle::Running,
            "it is fine; it just cannot"
        );
    }

    #[test]
    fn destroying_takes_the_disk_with_it() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        let room = overlay(&g.sandbox).parent().unwrap().to_path_buf();
        g.sandbox.start().unwrap();
        g.sandbox.destroy().unwrap();
        assert!(!room.exists());
    }

    fn kill(sandbox: &Sandbox) {
        sandbox
            .provider(|p: &mut HostSandbox| {
                let machine = p.machine_mut().unwrap();
                machine.child.kill().unwrap();
                machine.wait(Duration::from_secs(10)).unwrap();
            })
            .unwrap();
    }

    /// Killed from outside -- the out-of-memory killer, most often -- is a
    /// power cut: stopped, said why, and started again with its disk. As
    /// `failed` the only way on was a reset, which throws the disk away.
    #[test]
    fn a_guest_that_is_killed_from_outside_is_stopped_and_starts_again() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        kill(&g.sandbox);
        assert_eq!(g.sandbox.refresh().unwrap().lifecycle, Lifecycle::Stopped);
        // SIGKILL is what the kernel's out-of-memory killer sends, and it is
        // named as that. Windows has no SIGKILL.
        let expected = if cfg!(unix) {
            "host.guest_killed"
        } else {
            "host.guest_went"
        };
        assert_eq!(g.sandbox.state().reason.unwrap().code, expected);
        g.sandbox.start().unwrap();
        assert_eq!(g.sandbox.refresh().unwrap().lifecycle, Lifecycle::Running);
        g.sandbox.stop().unwrap();
    }

    struct Failing;

    impl Agent for Failing {
        fn sync(&mut self, _timeout: Duration) -> bool {
            true
        }
        fn interfaces(&mut self) -> Result<Vec<Value>, ControlError> {
            Ok(Vec::new())
        }
        fn run(&mut self, _: &str, _: &[Value], _: Option<Duration>) -> Result<Data, ControlError> {
            Err(ControlError::new(reason!(
                "machine.refused_command",
                command = "dir",
                detail = "not found"
            )))
        }
        fn read_file(&mut self, _: &str) -> Result<String, ControlError> {
            Ok(String::new())
        }
        fn write_file(&mut self, _: &str, _: &str) -> Result<i64, ControlError> {
            Ok(0)
        }
        fn close(&mut self) {}
    }

    /// Found on a real Windows guest: 'dir' is cmd.exe's own builtin, not a
    /// file BeatBox can launch, and the history showed "Ran dir" beside a
    /// card that had just refused exactly that.
    #[test]
    fn process_events_report_what_happened_not_what_was_asked_for() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        g.sandbox
            .provider(|p: &mut HostSandbox| p.set_agent(Box::new(Failing)))
            .unwrap();
        let outcome = g
            .sandbox
            .act("process.execute", data!({"command": "dir"}))
            .unwrap();
        assert!(!outcome.ok);
        assert!(g.log.matching("process.created").is_empty());
        assert!(g.log.matching("process.exited").is_empty());
    }

    /// Somebody chose Shut down in there. QEMU leaves with 0, and that is a
    /// machine at rest -- not a broken one.
    #[test]
    fn a_guest_shut_down_from_inside_is_stopped_and_starts_again() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        let s = &g.sandbox;
        s.start().unwrap();
        s.provider(|p: &mut HostSandbox| {
            p.monitor_mut().unwrap().quit().unwrap();
            p.machine_mut()
                .unwrap()
                .wait(Duration::from_secs(10))
                .unwrap();
        })
        .unwrap();
        assert_eq!(s.refresh().unwrap().lifecycle, Lifecycle::Stopped);
        assert_eq!(s.state().reason.unwrap().code, "host.guest_shut_down");
        assert_eq!(s.refresh().unwrap().lifecycle, Lifecycle::Stopped);
        s.start().unwrap();
        assert_eq!(s.refresh().unwrap().lifecycle, Lifecycle::Running);
    }

    /// VirtualBox's "Save the machine state": switched off, and the next
    /// start carries on from there -- once, then boots afresh again.
    #[test]
    fn a_saved_state_is_where_the_next_start_carries_on() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        let s = &g.sandbox;
        s.start().unwrap();
        answered(&s.act("snapshot.save_state", data!({})).unwrap());
        assert_eq!(s.refresh().unwrap().lifecycle, Lifecycle::Stopped);
        assert_eq!(s.state().reason.unwrap().code, "host.state_saved");
        assert_eq!(s.refresh().unwrap().lifecycle, Lifecycle::Stopped);
        assert_eq!(s.state().facts["saved_state"], true);
        assert_eq!(s.state().facts["snapshots"], json!([]), "not a save point");

        s.start().unwrap();
        assert_eq!(s.refresh().unwrap().lifecycle, Lifecycle::Running);
        assert_eq!(s.state().facts["saved_state"], false);
        let listed = Command::new("qemu-img")
            .args(["snapshot", "-U", "-l"])
            .arg(overlay(s))
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&listed.stdout);
        assert!(
            !listed.contains("beatbox-saved-state"),
            "forgotten once back: {listed}"
        );
        assert!(
            !s.act("snapshot.create", data!({"name": "beatbox-saved-state"}))
                .unwrap()
                .ok
        );
    }

    /// `loadvm` refuses a USB drive it does not find, and the guest tools
    /// drive is one: it is plugged in again before. Found 2026-10-14, the
    /// first real saved state broke the machine on its next start.
    #[test]
    fn a_saved_state_taken_with_a_drive_in_carries_on_with_it_in_again() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let room = Room::new("guest");
        let base = empty_disk(&room.join("base.qcow2"));
        let tools = room.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(tools.join("README.txt"), "tools").unwrap();
        let (runtime, _log) = build();
        let s = runtime
            .create(
                host(data!({"disk": base, "workspace": room.text("sbx"), "display": true}))
                    .named("target")
                    .with_resources(sized(1.0, 256.0)),
            )
            .unwrap();
        s.start().unwrap();
        answered(
            &s.act("media.insert", data!({"folder": room.text("tools")}))
                .unwrap(),
        );
        answered(&s.act("snapshot.save_state", data!({})).unwrap());
        assert_eq!(s.refresh().unwrap().lifecycle, Lifecycle::Stopped);

        s.start().unwrap();
        let facts = s.refresh().unwrap().facts;
        assert_eq!(facts["saved_state_lost"], false, "it carried on");
        assert_eq!(facts["saved_state"], false);

        // Without its drive the state cannot come back: booted afresh, said.
        answered(&s.act("snapshot.save_state", data!({})).unwrap());
        s.refresh().unwrap();
        std::fs::remove_dir_all(&tools).unwrap();
        s.start().unwrap();
        let facts = s.refresh().unwrap().facts;
        assert_eq!(s.state().lifecycle, Lifecycle::Running);
        assert_eq!(facts["saved_state_lost"], true);
        assert_eq!(facts["saved_state"], false, "and not tried again");
        s.stop().unwrap();
    }

    /// Without this, a guest this process itself stopped would look, to the
    /// next observation, exactly like one to reattach to.
    #[test]
    fn a_stopped_guest_leaves_no_socket_for_a_later_reattach_to_find() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        let socket = g
            .sandbox
            .provider(|p: &mut HostSandbox| p.monitor_socket().to_path_buf())
            .unwrap();
        g.sandbox.stop().unwrap();
        assert!(!socket.exists());
    }

    /// The point of the whole feature: BeatBox closes with the guest left
    /// running and reopens as a new process, which knows this sandbox only by
    /// what was written down. It has to find the same real QEMU by its socket
    /// alone, and be able to drive it.
    #[test]
    fn a_guest_is_reattached_by_a_process_that_did_not_start_it() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        // QEMU's QMP server takes one client at a time, and closing this is
        // what a process actually exiting does for free.
        g.sandbox
            .provider(|p: &mut HostSandbox| p.forget_channels())
            .unwrap();

        let (next_run, _) = build();
        let reattached = next_run
            .restore(
                g.sandbox.identity(),
                g.sandbox.config(),
                Lifecycle::Running,
                None,
            )
            .unwrap();
        assert_eq!(reattached.lifecycle(), Lifecycle::Running);
        assert_eq!(reattached.state().facts["status"], "running");
        reattached.pause().unwrap();
        assert_eq!(status(&reattached), "paused");
        reattached.resume().unwrap();

        // Not just observed -- controlled: quit over the reattached monitor
        // has to reach the actual process the first runtime started.
        reattached.stop().unwrap();
        assert_eq!(reattached.lifecycle(), Lifecycle::Stopped);
        let ended = g
            .sandbox
            .provider(|p: &mut HostSandbox| p.machine_mut().unwrap().wait(Duration::from_secs(10)))
            .unwrap()
            .expect("the original QEMU exited");
        assert_eq!(ended.code(), Some(0));
        drop(next_run);
        drop(g.runtime);
    }

    /// The OOM killer, or anything else with nobody there to see it happen:
    /// a stale socket file cannot pretend anyone is listening on it.
    #[test]
    fn a_guest_that_died_while_nobody_was_watching_is_found_gone() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        kill(&g.sandbox);
        let (next_run, _) = build();
        let lost = next_run
            .restore(
                g.sandbox.identity(),
                g.sandbox.config(),
                Lifecycle::Running,
                None,
            )
            .unwrap();
        assert_eq!(lost.lifecycle(), Lifecycle::Failed);
        assert_eq!(lost.state().reason.unwrap().code, "host.guest_lost");
        // Put back as it was made, it is a machine at rest again, not one
        // still being looked for through the dead guest's sockets.
        assert_eq!(lost.reset().unwrap().lifecycle, Lifecycle::Created);
        assert_ne!(lost.lifecycle(), Lifecycle::Failed);
    }

    /// They live in the overlay, and so does the list of them.
    #[test]
    fn save_points_survive_beatbox_closing() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        assert!(
            g.sandbox
                .act("snapshot.create", data!({"name": "before the attack"}))
                .unwrap()
                .ok
        );
        g.sandbox.stop().unwrap();

        let (next_run, _) = build();
        let restored = next_run
            .restore(
                g.sandbox.identity(),
                g.sandbox.config(),
                Lifecycle::Stopped,
                None,
            )
            .unwrap();
        assert_eq!(
            restored.state().facts["snapshots"],
            json!(["before the attack"])
        );
        restored.start().unwrap();
        assert!(
            restored
                .act("snapshot.restore", data!({"name": "before the attack"}))
                .unwrap()
                .ok
        );
        restored.stop().unwrap();
    }

    /// A guest this process started, cabled as `network` says.
    fn guest_on(network: &str) -> Guest {
        let room = Room::new("guest-net");
        let base = empty_disk(&room.join("base.qcow2"));
        let (runtime, log) = build();
        let sandbox = runtime
            .create(
                host(data!({"disk": base, "workspace": room.text("sbx"), "network": network}))
                    .named("target")
                    .with_resources(sized(1.0, 256.0)),
            )
            .unwrap();
        Guest {
            sandbox,
            runtime,
            log,
            room,
        }
    }

    /// Stopped, moved to `network`, started again.
    fn recable(g: &Guest, network: &str) {
        g.sandbox.stop().unwrap();
        let mut settings = g.sandbox.config().settings;
        settings.insert("network".into(), json!(network));
        g.sandbox
            .reconfigure(host(settings).with_resources(sized(1.0, 256.0)))
            .unwrap();
        g.sandbox.start().unwrap();
    }

    /// QEMU loads a save point only onto the devices it was taken on, and
    /// finds out after it has put the disk back. A save point from another
    /// network is refused before that, and the guest runs on untouched.
    #[test]
    fn a_save_point_from_another_network_is_refused_before_anything_is_touched() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest_on("isolated");
        g.sandbox.start().unwrap();
        assert!(
            g.sandbox
                .act("snapshot.create", data!({"name": "cabled"}))
                .unwrap()
                .ok
        );
        recable(&g, "none");

        let outcome = g
            .sandbox
            .act("snapshot.restore", data!({"name": "cabled"}))
            .unwrap();
        assert!(!outcome.ok);
        let why = outcome.reason.unwrap();
        assert_eq!(why.code, "machine.snapshot_setup_differs");
        assert_eq!(
            (
                why.values["setting"].as_str(),
                why.values["was"].as_str(),
                why.values["now"].as_str()
            ),
            ("network", "isolated", "none")
        );
        assert_eq!(g.sandbox.refresh().unwrap().lifecycle, Lifecycle::Running);
        assert_eq!(status(&g.sandbox), "running");
    }

    /// Isolated and NAT are the same card to QEMU: going between them is not
    /// a reason to refuse.
    #[test]
    fn isolated_and_nat_are_the_same_hardware_to_a_save_point() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest_on("isolated");
        g.sandbox.start().unwrap();
        assert!(
            g.sandbox
                .act("snapshot.create", data!({"name": "either"}))
                .unwrap()
                .ok
        );
        recable(&g, "nat");
        assert!(
            g.sandbox
                .act("snapshot.restore", data!({"name": "either"}))
                .unwrap()
                .ok
        );
        g.sandbox.stop().unwrap();
    }

    /// A save point older than the record of its hardware is tried. When
    /// QEMU gets half way -- the disk back, the memory not -- the guest is
    /// switched off rather than left paused one Resume away from ruining its
    /// disk, and says so when it is next looked at.
    #[test]
    fn a_restore_that_fails_half_way_switches_the_guest_off() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest_on("isolated");
        g.sandbox.start().unwrap();
        assert!(
            g.sandbox
                .act("snapshot.create", data!({"name": "old"}))
                .unwrap()
                .ok
        );
        recable(&g, "none");
        g.sandbox.stop().unwrap();
        // As a save point taken before BeatBox wrote the record down.
        std::fs::remove_file(g.room.join("sbx/save-points.json")).unwrap();
        let (next_run, _) = build();
        let restored = next_run
            .restore(
                g.sandbox.identity(),
                g.sandbox.config(),
                Lifecycle::Stopped,
                None,
            )
            .unwrap();
        restored.start().unwrap();

        let outcome = restored
            .act("snapshot.restore", data!({"name": "old"}))
            .unwrap();
        assert!(!outcome.ok);
        assert_eq!(
            outcome.reason.unwrap().code,
            "machine.snapshot_restore_failed"
        );
        assert_eq!(restored.refresh().unwrap().lifecycle, Lifecycle::Stopped);
        let why = restored.state().reason.unwrap();
        assert_eq!(why.code, "host.stopped_after_failed_restore");
        assert_eq!(why.values["name"], "old");
        drop(next_run);
        drop(g.runtime);
    }

    /// Stop asks first: the power button, then its grace, and only then the
    /// plug. A disk with nothing on it cannot hear the button, so it is
    /// switched off -- after the grace, not before.
    #[test]
    fn a_guest_that_ignores_the_power_button_is_switched_off_after_its_grace() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        let asked = std::time::Instant::now();
        g.sandbox.stop().unwrap();
        assert!(
            asked.elapsed() >= support::GRACE,
            "stopped without asking first"
        );
        assert_eq!(g.sandbox.lifecycle(), Lifecycle::Stopped);
    }

    #[test]
    fn a_cleanly_stopped_guest_restores_as_stopped_not_running() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let g = guest();
        g.sandbox.start().unwrap();
        g.sandbox.stop().unwrap();
        let (next_run, _) = build();
        let restored = next_run
            .restore(
                g.sandbox.identity(),
                g.sandbox.config(),
                Lifecycle::Stopped,
                None,
            )
            .unwrap();
        assert_eq!(restored.lifecycle(), Lifecycle::Stopped);
    }

    /// Unescaped, QEMU reads everything after the comma as another option
    /// and refuses to start.
    #[test]
    fn a_real_guest_boots_from_a_folder_with_a_comma_in_its_name() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let room = Room::new("comma-real");
        let folder = room.join("Lab, test");
        std::fs::create_dir_all(&folder).unwrap();
        let base = empty_disk(&folder.join("base.qcow2"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(
                host(data!({"disk": base, "workspace": folder.join("sbx, one").to_string_lossy()}))
                    .with_resources(sized(1.0, 256.0)),
            )
            .unwrap();
        assert_eq!(sandbox.start().unwrap().lifecycle, Lifecycle::Running);
        assert_eq!(status(&sandbox), "running");
    }
}

/// A folder plugged into a running guest as a read-only USB drive, against a
/// bare QEMU with the one USB controller a windowed machine gets -- rather
/// than a HostSandbox with a window, which would open one on whoever is
/// running the tests.
mod media_tests {
    use super::*;

    struct Bare {
        qemu: std::process::Child,
        monitor: Monitor,
        room: Room,
    }

    impl Drop for Bare {
        fn drop(&mut self) {
            let _ = self.qemu.kill();
            let _ = self.qemu.wait();
        }
    }

    fn bare() -> Bare {
        let room = Room::new("media");
        std::fs::create_dir_all(room.join("tools")).unwrap();
        std::fs::write(room.join("tools/setup.exe"), b"MZ not really").unwrap();
        let disk = empty_disk(&room.join("d.qcow2"));
        let socket = room.join("m.sock");
        let endpoint = beatbox_isolation::endpoint::prepare(&socket).unwrap();
        let qemu = Command::new("qemu-system-x86_64")
            .args([
                "-M",
                "pc",
                "-m",
                "64",
                "-display",
                "none",
                "-device",
                "qemu-xhci,id=usb0",
            ])
            .arg("-drive")
            .arg(format!("file={disk},if=none,id=sysdisk"))
            .args(["-device", "ide-hd,drive=sysdisk", "-qmp"])
            .arg(beatbox_isolation::endpoint::qmp_argument(&endpoint))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let monitor = Monitor::connect(&socket, Duration::from_secs(10)).unwrap();
        Bare {
            qemu,
            monitor,
            room,
        }
    }

    #[test]
    fn it_is_plugged_in_once_and_a_second_time_is_no_error() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let mut b = bare();
        let tools = b.room.text("tools");
        assert!(b.monitor.insert_media(&tools, "BEATBOX").unwrap());
        assert!(!b.monitor.insert_media(&tools, "BEATBOX").unwrap());
        let nodes = b.monitor.execute("query-named-block-nodes", None).unwrap();
        let names: Vec<&str> = nodes
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|node| node["node-name"].as_str())
            .collect();
        assert!(names.contains(&"bbmedia"));
    }

    fn nodes(monitor: &mut Monitor) -> Vec<String> {
        let nodes = monitor.execute("query-named-block-nodes", None).unwrap();
        nodes
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|node| node["node-name"].as_str().map(str::to_owned))
            .collect()
    }

    /// The tools' drive is plugged in at every windowed start; a pen of the
    /// person's own has a slot of its own beside it rather than being
    /// silently refused as "already there".
    #[test]
    fn a_pen_and_the_tools_drive_are_in_at_once() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let mut b = bare();
        std::fs::create_dir_all(b.room.join("pen")).unwrap();
        std::fs::write(b.room.join("pen/sample.txt"), b"hello").unwrap();
        assert!(
            b.monitor
                .insert_media(&b.room.text("tools"), "BEATBOX")
                .unwrap()
        );
        assert!(b.monitor.insert_pen(&b.room.text("pen"), "PEN").unwrap());
        let names = nodes(&mut b.monitor);
        assert!(names.contains(&"bbmedia".to_owned()) && names.contains(&"bbpen".to_owned()));
    }

    /// Out, and in again: the drive behind a pulled pen is let go of, so the
    /// same slot takes the next one.
    #[test]
    fn a_pen_comes_out_and_goes_back_in() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let mut b = bare();
        std::fs::create_dir_all(b.room.join("pen")).unwrap();
        assert!(b.monitor.insert_pen(&b.room.text("pen"), "PEN").unwrap());
        b.monitor.eject_pen().unwrap();
        assert!(!nodes(&mut b.monitor).contains(&"bbpen".to_owned()));
        assert!(b.monitor.insert_pen(&b.room.text("pen"), "PEN").unwrap());
        b.monitor.eject_pen().unwrap();
        // And pulling one that is not in is not an error.
        b.monitor.eject_pen().unwrap();
    }

    /// savevm refuses a writable disk that cannot be snapshotted. This one is
    /// read-only at the block layer, so it is skipped rather than refused.
    #[test]
    fn save_points_still_work_with_it_plugged_in() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let mut b = bare();
        let tools = b.room.text("tools");
        b.monitor.insert_media(&tools, "BEATBOX").unwrap();
        b.monitor.snapshot_save("with the drive in").unwrap();
        b.monitor.snapshot_load("with the drive in").unwrap();
    }
}

/// An agent that starts after the machine did -- at the end of a boot, or
/// the moment somebody installs the guest tools. Found 2026-09-23:
/// installed, answering on its socket, and never heard from.
mod late_agent_tests {
    use super::*;

    struct Answering;

    impl Agent for Answering {
        fn sync(&mut self, _timeout: Duration) -> bool {
            true
        }
        fn interfaces(&mut self) -> Result<Vec<Value>, ControlError> {
            Ok(Vec::new())
        }
        fn run(&mut self, _: &str, _: &[Value], _: Option<Duration>) -> Result<Data, ControlError> {
            Ok(Data::new())
        }
        fn read_file(&mut self, _: &str) -> Result<String, ControlError> {
            Ok(String::new())
        }
        fn write_file(&mut self, _: &str, _: &str) -> Result<i64, ControlError> {
            Ok(0)
        }
        fn close(&mut self) {}
    }

    fn started() -> (Sandbox, Guarded, Room) {
        let room = Room::new("late");
        let base = empty_disk(&room.join("base.qcow2"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(
                host(data!({"disk": base, "workspace": room.text("sbx")}))
                    .named("late")
                    .with_resources(sized(1.0, 256.0)),
            )
            .unwrap();
        sandbox.start().unwrap();
        assert_eq!(
            sandbox.refresh().unwrap().facts["agent"],
            false,
            "an empty disk has no agent"
        );
        (sandbox, runtime, room)
    }

    fn arrives(sandbox: &Sandbox, every: Duration) -> Arc<AtomicUsize> {
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        sandbox
            .provider(move |p: &mut HostSandbox| {
                let environment = p.environment_mut();
                environment.agent_retry_every = every;
                environment.connect_agent = Arc::new(move |_: &Path, _| {
                    counted.fetch_add(1, Ordering::Relaxed);
                    Ok(Box::new(Answering) as Box<dyn Agent>)
                });
            })
            .unwrap();
        asked
    }

    #[test]
    fn an_agent_that_turns_up_later_is_found_by_the_next_look() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let (sandbox, _runtime, _room) = started();
        arrives(&sandbox, Duration::ZERO);
        assert_eq!(sandbox.refresh().unwrap().facts["agent"], true);
    }

    /// A failed ask waits out its whole timeout; a screen polling every few
    /// seconds must not pay that on every poll.
    #[test]
    fn it_is_not_asked_on_every_single_look() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let (sandbox, _runtime, _room) = started();
        let asked = arrives(&sandbox, Duration::from_secs(3600));
        sandbox.refresh().unwrap();
        assert_eq!(asked.load(Ordering::Relaxed), 0);
    }
}

/// An agent that stops answering is let go of -- connection and all.
mod agent_connection_tests {
    use super::*;

    struct Scripted {
        fails: bool,
        closed: Arc<Mutex<u32>>,
    }

    impl Agent for Scripted {
        fn sync(&mut self, _timeout: Duration) -> bool {
            true
        }
        fn interfaces(&mut self) -> Result<Vec<Value>, ControlError> {
            if self.fails {
                return Err(ControlError::new(reason!(
                    "machine.channel_gone",
                    detail = "closed"
                )));
            }
            Ok(Vec::new())
        }
        fn run(&mut self, _: &str, _: &[Value], _: Option<Duration>) -> Result<Data, ControlError> {
            Ok(Data::new())
        }
        fn read_file(&mut self, _: &str) -> Result<String, ControlError> {
            Ok(String::new())
        }
        fn write_file(&mut self, _: &str, _: &str) -> Result<i64, ControlError> {
            Ok(0)
        }
        fn close(&mut self) {
            *self.closed.lock().unwrap() += 1;
        }
    }

    fn disc_sandbox() -> (Sandbox, Guarded, Room) {
        let room = Room::new("agentconn");
        let disc = fake_disc(&room.join("d.iso"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(host(data!({"disk": disc, "workspace": room.text("sbx")})))
            .unwrap();
        (sandbox, runtime, room)
    }

    /// QEMU serves the agent channel to one client at a time. Forgotten
    /// without being closed, the dead connection kept the only slot, and
    /// every later attempt to reach the agent was refused.
    #[test]
    fn a_dropped_agent_is_closed_not_just_forgotten() {
        let (sandbox, _runtime, _room) = disc_sandbox();
        let closed = Arc::new(Mutex::new(0));
        let agent = Scripted {
            fails: true,
            closed: Arc::clone(&closed),
        };
        let (facts, still) = sandbox
            .provider(move |p: &mut HostSandbox| {
                p.set_agent(Box::new(agent));
                (p.guest_facts(), p.has_agent())
            })
            .unwrap();
        assert_eq!(facts, data!({"agent": false}));
        assert_eq!(*closed.lock().unwrap(), 1);
        assert!(!still);
    }

    /// 'No such program' is the agent answering. Only a broken channel is a
    /// reason to let go of one.
    #[test]
    fn a_refusal_through_a_working_agent_keeps_it() {
        let (sandbox, _runtime, _room) = disc_sandbox();
        let agent = Scripted {
            fails: false,
            closed: Arc::default(),
        };
        let kept = sandbox
            .provider(move |p: &mut HostSandbox| {
                p.set_agent(Box::new(agent));
                p.agent_failed(&ControlError::new(reason!(
                    "machine.refused_command",
                    command = "x",
                    detail = "y"
                )));
                p.has_agent()
            })
            .unwrap();
        assert!(kept);
    }
}

/// What a person is shown about a machine's traffic, and where it is kept.
mod traffic_tests {
    use super::*;
    use beatbox_host::sandbox::{TRAFFIC, is_outside};

    fn frame(protocol: u8, to: [u8; 4], port: u16, payload: &[u8]) -> Vec<u8> {
        let mut transport = vec![0x9c, 0x40];
        transport.extend(port.to_be_bytes());
        if protocol == 17 {
            transport.extend((8 + payload.len() as u16).to_be_bytes());
            transport.extend([0u8; 2]);
        } else {
            transport.extend([0u8; 8]);
            transport.extend([0x50, 0x02]);
            transport.extend([0u8; 6]);
        }
        transport.extend(payload);
        let mut ip = vec![0x45, 0x00];
        ip.extend((20 + transport.len() as u16).to_be_bytes());
        ip.extend([0u8; 5]);
        ip.push(protocol);
        ip.extend([0u8; 2]);
        ip.extend([10, 0, 2, 15]);
        ip.extend(to);
        ip.extend(transport);
        let mut ethernet = vec![0xaa; 12];
        ethernet.extend(0x0800u16.to_be_bytes());
        ethernet.extend(ip);
        ethernet
    }

    fn pcap(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(0xA1B2_C3D4u32.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(4u16.to_le_bytes());
        for word in [0u32, 0, 65535, 1] {
            out.extend(word.to_le_bytes());
        }
        for frame in frames {
            for word in [0u32, 0, frame.len() as u32, frame.len() as u32] {
                out.extend(word.to_le_bytes());
            }
            out.extend(frame);
        }
        out
    }

    fn dns(name: &str) -> Vec<u8> {
        let mut body = Vec::new();
        for field in [0x1234u16, 0x0100, 1, 0, 0, 0] {
            body.extend(field.to_be_bytes());
        }
        for label in name.split('.') {
            body.push(label.len() as u8);
            body.extend(label.as_bytes());
        }
        body.extend([0, 0, 1, 0, 1]);
        body
    }

    fn made(room: &Room, capture: Value) -> (Sandbox, Guarded) {
        let (runtime, _) = build();
        let disc = fake_disc(&room.join("installer.iso"));
        let settings = data!({"disk": disc, "workspace": room.text("sbx"), "network": "isolated",
                              "capture": capture});
        let made = runtime.create(host(settings).named("target")).unwrap();
        (made, runtime)
    }

    #[test]
    fn outside_is_somewhere_beyond_this_computer_and_its_sandbox_network() {
        for address in ["93.184.216.34", "8.8.8.8", "2606:4700::1111"] {
            assert!(is_outside(address), "{address}");
        }
        for address in [
            "10.0.2.15",
            "10.0.2.3",
            "10.0.2.255",
            "192.168.1.1",
            "127.0.0.1",
            "239.255.255.250",
            "255.255.255.255",
            "169.254.1.1",
            "ff02::1:2",
            "fe80::1",
            "fd00::1",
            "broadcast",
        ] {
            assert!(!is_outside(address), "{address}");
        }
    }

    /// Nobody should have to choose a path: `true` is a file in the
    /// machine's own room, which goes when the machine does.
    #[test]
    fn capture_true_is_a_file_in_the_machines_room() {
        let room = Room::new("traffic");
        let (on, _on) = made(&room, json!(true));
        assert_eq!(
            spec_of(&on).capture_path,
            room.join("sbx").join(TRAFFIC).to_string_lossy()
        );
        let room = Room::new("traffic");
        let (off, _off) = made(&room, json!(false));
        assert_eq!(spec_of(&off).capture_path, "");
        let room = Room::new("traffic");
        let (there, _there) = made(&room, json!("/somewhere/else.pcap"));
        assert_eq!(spec_of(&there).capture_path, "/somewhere/else.pcap");
    }

    /// The names it asked for, where outside it tried to go, and everything
    /// else only counted: its DNS server, its own replies, a local broadcast.
    #[test]
    fn the_summary_separates_outside_from_the_local_network() {
        let room = Room::new("traffic");
        let (sandbox, _runtime) = made(&room, json!(true));
        assert!(sandbox.capabilities().supports("network.traffic"));
        let before = sandbox.act("network.traffic", Data::new()).unwrap();
        answered(&before);
        assert_eq!(before.data["recording"], true);
        assert_eq!(before.data["file"], "");

        let frames = [
            frame(17, [10, 0, 2, 3], 53, &dns("update-check.example")),
            frame(6, [93, 184, 216, 34], 443, b""),
            frame(6, [93, 184, 216, 34], 443, b""),
            frame(17, [239, 255, 255, 250], 1900, b"M-SEARCH"),
        ];
        std::fs::write(spec_of(&sandbox).capture_path, pcap(&frames)).unwrap();
        let after = sandbox.act("network.traffic", Data::new()).unwrap();
        answered(&after);
        let seen = &after.data;
        assert_eq!(seen["names"], json!(["update-check.example"]));
        assert_eq!(seen["outside"].as_array().unwrap().len(), 1);
        assert_eq!(seen["outside"][0]["address"], "93.184.216.34");
        assert_eq!(seen["outside"][0]["port"], 443);
        assert_eq!(seen["outside"][0]["packets"], 2);
        assert_eq!(seen["local"], 2);
        assert_eq!(seen["packets"], 4);
    }

    #[test]
    fn a_machine_not_recording_says_so() {
        let room = Room::new("traffic");
        let (sandbox, _runtime) = made(&room, json!(false));
        let answer = sandbox.act("network.traffic", Data::new()).unwrap();
        assert_eq!(answer.data["recording"], false);
    }
}

/// What is running inside, and ending it.
mod process_tests {
    use super::*;
    use beatbox_host::{folder_size, parse_tasklist};

    type Runs = Arc<Mutex<Vec<(String, Vec<Value>)>>>;

    /// An agent that says what it is told to, and keeps what it was asked
    /// to run.
    struct Scripted {
        runs: Runs,
        stdout: String,
        exit_code: i64,
        os: Value,
        asked_os: Arc<AtomicUsize>,
    }

    impl Agent for Scripted {
        fn sync(&mut self, _timeout: Duration) -> bool {
            true
        }
        fn interfaces(&mut self) -> Result<Vec<Value>, ControlError> {
            Ok(Vec::new())
        }
        fn run(
            &mut self,
            path: &str,
            arguments: &[Value],
            _: Option<Duration>,
        ) -> Result<Data, ControlError> {
            self.runs
                .lock()
                .unwrap()
                .push((path.to_owned(), arguments.to_vec()));
            Ok(data!({"stdout": self.stdout, "exit_code": self.exit_code}))
        }
        fn read_file(&mut self, _: &str) -> Result<String, ControlError> {
            Ok(String::new())
        }
        fn write_file(&mut self, _: &str, _: &str) -> Result<i64, ControlError> {
            Ok(0)
        }
        fn os(&mut self) -> Result<Value, ControlError> {
            self.asked_os.fetch_add(1, Ordering::SeqCst);
            Ok(self.os.clone())
        }
        fn close(&mut self) {}
    }

    struct Running {
        sandbox: Sandbox,
        _runtime: Guarded,
        _room: Room,
        runs: Runs,
        asked_os: Arc<AtomicUsize>,
    }

    fn running(os: Value, stdout: &str, exit_code: i64) -> Running {
        let room = Room::new("procs");
        let base = empty_disk(&room.join("base.qcow2"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(
                host(data!({"disk": base, "workspace": room.text("sbx")}))
                    .named("procs")
                    .with_resources(sized(1.0, 256.0)),
            )
            .unwrap();
        sandbox.start().unwrap();
        let runs = Runs::default();
        let asked_os = Arc::new(AtomicUsize::new(0));
        let agent = Scripted {
            runs: Arc::clone(&runs),
            stdout: stdout.to_owned(),
            exit_code,
            os,
            asked_os: Arc::clone(&asked_os),
        };
        sandbox
            .provider(|p: &mut HostSandbox| p.set_agent(Box::new(agent)))
            .unwrap();
        Running {
            sandbox,
            _runtime: runtime,
            _room: room,
            runs,
            asked_os,
        }
    }

    /// The Linux agent creates files anyone in the guest may change; a file
    /// written there is left changeable by its owner alone. Windows has
    /// no chmod, and its own rules.
    #[test]
    fn a_written_file_is_left_changeable_by_its_owner_alone() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let linux = running(json!({"id": "ubuntu"}), "", 0);
        let wrote = linux
            .sandbox
            .act(
                "filesystem.write",
                data!({"path": "/srv/run.sh", "content": "echo hi"}),
            )
            .unwrap();
        answered(&wrote);
        assert_eq!(
            linux.runs.lock().unwrap().as_slice(),
            [(
                "chmod".to_owned(),
                vec![json!("go-w"), json!("--"), json!("/srv/run.sh")]
            )]
        );
        drop(linux);
        let windows = running(json!({"id": "mswindows"}), "", 0);
        let wrote = windows
            .sandbox
            .act(
                "filesystem.write",
                data!({"path": r"C:\x.txt", "content": "hi"}),
            )
            .unwrap();
        answered(&wrote);
        assert!(windows.runs.lock().unwrap().is_empty());
    }

    #[test]
    fn tasklist_is_read_into_the_same_shape_as_ps() {
        let text = "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\",\"Unknown\",\"NT AUTHORITY\\SYSTEM\",\"12:00:00\",\"N/A\"\r\n\
                    \"notepad.exe\",\"4312\",\"Console\",\"1\",\"12,345 K\",\"Running\",\"DESKTOP\\ana\",\"0:00:01\",\"Untitled - Notepad\"\r\n";
        let rows = parse_tasklist(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["command"], "notepad.exe");
        assert_eq!(rows[1]["pid"], 4312);
        assert_eq!(rows[1]["user"], "DESKTOP\\ana");
        assert_eq!(rows[1]["state"], "Running");
    }

    #[test]
    fn a_folder_is_counted_only_as_far_as_the_limit() {
        let room = Room::new("size");
        std::fs::create_dir_all(room.join("a/b")).unwrap();
        std::fs::write(room.join("a/one"), vec![0u8; 100]).unwrap();
        std::fs::write(room.join("a/b/two"), vec![0u8; 50]).unwrap();
        assert_eq!(folder_size(&room.join("a"), 1000), 150);
        assert!(folder_size(&room.join("a"), 120) > 120);
    }

    /// A machine nobody described is asked what it is, once a run: Windows
    /// is listed by tasklist, and everything else by ps.
    #[test]
    fn the_guest_is_asked_what_it_is_and_listed_by_its_own_tool() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let windows = running(
            json!({"id": "mswindows"}),
            "\"notepad.exe\",\"4312\",\"Console\",\"1\",\"1 K\",\"Running\",\"u\",\"0\",\"t\"",
            0,
        );
        let listed = windows.sandbox.act("process.list", Data::new()).unwrap();
        answered(&listed);
        assert_eq!(listed.data["processes"][0]["command"], "notepad.exe");
        windows.sandbox.act("process.list", Data::new()).unwrap();
        assert!(windows.runs.lock().unwrap()[0].0.ends_with("tasklist.exe"));
        assert_eq!(
            windows.asked_os.load(Ordering::SeqCst),
            1,
            "asked once a run, not every look"
        );
        drop(windows);

        let linux = running(Value::Null, "1 0 root S systemd", 0);
        linux.sandbox.act("process.list", Data::new()).unwrap();
        assert_eq!(linux.runs.lock().unwrap()[0].0, "/bin/ps");
    }

    #[test]
    fn a_process_is_ended_by_its_number_and_nothing_else() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let linux = running(Value::Null, "", 0);
        let ended = linux
            .sandbox
            .act("process.kill", data!({"pid": 1234}))
            .unwrap();
        answered(&ended);
        assert_eq!(
            linux.runs.lock().unwrap()[0],
            ("/bin/kill".to_owned(), vec![json!("-KILL"), json!("1234")])
        );
        for pid in [json!(1), json!(0), json!(-5)] {
            let refused = linux
                .sandbox
                .act("process.kill", data!({"pid": pid}))
                .unwrap();
            assert_eq!(refused.reason.unwrap().code, "host.bad_pid");
        }
        // Not a number at all: the runtime refuses it before the host sees it.
        let refused = linux
            .sandbox
            .act("process.kill", data!({"pid": "1; reboot"}))
            .unwrap();
        assert_eq!(refused.reason.unwrap().code, "engine.bad_argument");
        assert_eq!(
            linux.runs.lock().unwrap().len(),
            1,
            "nothing refused was run"
        );
        drop(linux);

        let windows = running(json!({"id": "mswindows"}), "", 128);
        let refused = windows
            .sandbox
            .act("process.kill", data!({"pid": 4312}))
            .unwrap();
        assert_eq!(refused.reason.unwrap().code, "host.kill_refused");
        let (program, arguments) = windows.runs.lock().unwrap()[0].clone();
        assert!(program.ends_with("taskkill.exe"));
        assert_eq!(arguments, vec![json!("/PID"), json!("4312"), json!("/F")]);
    }
}

/// A machine that starts in UEFI, and one that cannot because this computer
/// has no UEFI firmware.
mod uefi_tests {
    use super::*;
    use beatbox_host::sandbox::EFI_VARS;
    use beatbox_isolation::firmware::find_uefi;

    #[test]
    fn with_no_uefi_firmware_here_the_refusal_says_what_to_install() {
        let room = Room::new("uefi");
        let base = fake_disc(&room.join("installer.iso"));
        let mut environment = with_a_screen();
        environment.uefi = Arc::new(|_| None);
        let (runtime, _) = build_with(environment);
        let made = runtime.create(host(
            data!({"disk": base, "workspace": room.text("sbx"), "firmware": "uefi"}),
        ));
        let Err(EngineError::Provider(error)) = made else {
            panic!("expected a refusal, got {made:?}");
        };
        let why = error.reason.unwrap();
        assert_eq!(why.code, "host.no_uefi");
        assert!(!why.values["command"].is_empty());
    }

    /// OVMF's two flash chips on the command line, the variables in the
    /// machine's own room -- as qcow2, so that a save point, which QEMU
    /// refuses beside a writable raw drive, still works -- and gone with a
    /// reset, since they belonged to the disk it threw away.
    #[test]
    fn a_uefi_machine_starts_on_ovmf_and_can_still_be_saved() {
        needs_qemu!();
        if find_uefi(&qemu()).is_none() {
            eprintln!("SKIPPED: no OVMF on this computer");
            return;
        }
        let _one = one_guest_at_a_time();
        let room = Room::new("uefi");
        let base = empty_disk(&room.join("base.qcow2"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(
                host(data!({"disk": base, "workspace": room.text("sbx"), "firmware": "uefi"}))
                    .named("uefi")
                    .with_resources(sized(1.0, 256.0)),
            )
            .unwrap();
        assert!(argv(&sandbox).contains("if=pflash"));
        sandbox.start().unwrap();
        assert!(room.join("sbx").join(EFI_VARS).is_file());
        let saved = sandbox
            .act("snapshot.create", data!({"name": "under-uefi"}))
            .unwrap();
        assert!(saved.ok, "{:?}", saved.reason);
        sandbox.reset().unwrap();
        assert!(!room.join("sbx").join(EFI_VARS).exists());
        sandbox.destroy().unwrap();
    }

    /// VirtualBox's own format is a base like any other: the machine runs on
    /// a qcow2 layer over it, and the .vdi is the same afterwards, byte for
    /// byte.
    #[test]
    fn a_machine_runs_straight_from_a_vdi_and_leaves_it_alone() {
        needs_qemu!();
        let _one = one_guest_at_a_time();
        let room = Room::new("vdi");
        let vdi = room.join("machine.vdi");
        let made = Command::new("qemu-img")
            .args(["create", "-q", "-f", "vdi"])
            .arg(&vdi)
            .arg("64M")
            .status()
            .unwrap();
        assert!(made.success());
        let before = std::fs::read(&vdi).unwrap();
        let (runtime, _) = build();
        let sandbox = runtime
            .create(
                host(data!({"disk": vdi.to_string_lossy(), "workspace": room.text("sbx")}))
                    .named("vdi")
                    .with_resources(sized(1.0, 256.0)),
            )
            .unwrap();
        sandbox.start().unwrap();
        assert_eq!(sandbox.state().lifecycle, Lifecycle::Running);
        sandbox.destroy().unwrap();
        assert_eq!(std::fs::read(&vdi).unwrap(), before);
    }

    /// Unset, a disk nobody described is read: an empty disk has no GPT and
    /// is the BIOS, as every machine was before.
    #[test]
    fn a_disk_with_nothing_on_it_is_bios() {
        needs_qemu!();
        let room = Room::new("uefi");
        let base = empty_disk(&room.join("base.qcow2"));
        let (runtime, _) = build();
        let sandbox = runtime
            .create(host(data!({"disk": base, "workspace": room.text("sbx")})))
            .unwrap();
        assert!(!argv(&sandbox).contains("pflash"));
        assert_eq!(sandbox.state().facts["firmware"], "bios");
        assert_eq!(
            sandbox.state().facts["bootable"],
            false,
            "said, rather than left to iPXE"
        );
    }
}
