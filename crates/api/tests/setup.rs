//! What BeatBox needs from this computer, and what it runs to put it there.
//!
//! No test here runs a fix: every one is answered by `support::Answers`,
//! which keeps the argv. What is checked is which command each computer
//! gets, and what each answer is read as.

mod support;

use std::sync::Arc;
use std::time::Duration;

use beatbox_api::setup::{
    ACCELERATION, GUEST_TOOLS, Host, QEMU, RELOGIN, Step, acceleration_step, guest_tools_step,
    in_group, outcome, qemu_step,
};
use beatbox_api::{Api, ApiParts};
use beatbox_contract::SetupView;
use beatbox_isolation::runner::Output;
use serde_json::json;
use support::{Answers, Room, bare_arch, call, code, data, job, parts, settle, setup};

fn with(room: &Room, host: Host, runner: Arc<Answers>) -> Api {
    Api::open(ApiParts {
        setup: setup(host, runner),
        ..parts(room.path())
    })
}

fn debian() -> Host {
    Host {
        os_release: "ID=ubuntu\nID_LIKE=debian\n".into(),
        ..bare_arch()
    }
}

fn windows() -> Host {
    let mut host = bare_arch();
    host.windows = true;
    host.kvm.windows = true;
    host.kvm.linux = false;
    host.kvm.device = false;
    host.pkexec = false;
    host.winget = true;
    host.os_release = String::new();
    host
}

fn fixed_by(step: &Step) -> Vec<&str> {
    step.fix
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect()
}

#[test]
fn test_three_steps_in_order_each_explaining_itself() {
    let room = Room::new("setup");
    let api = support::open(room.path());
    let status = data(call(&api, "setup.status", json!({})));
    let view: SetupView = serde_json::from_value(status).unwrap();
    let keys: Vec<&str> = view.steps.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(keys, [QEMU, ACCELERATION, GUEST_TOOLS]);
    for step in &view.steps {
        assert!(!step.reason.code.is_empty(), "{}", step.key);
    }
    assert!(
        view.steps[2].optional,
        "the guest tools are only for Windows guests"
    );
    assert!(!view.steps[0].optional && !view.steps[1].optional);
}

#[test]
fn test_qemu_with_a_window_is_ready() {
    let host = Host {
        qemu: true,
        window: true,
        ..bare_arch()
    };
    let step = qemu_step(&host);
    assert!(step.view.ready);
    assert!(!step.view.fixable);
    assert_eq!(step.view.reason.code, "setup.qemu.ready");
}

/// The emulator, `qemu-img` and the window, in one command, as root through
/// the desktop's own password dialog.
#[test]
fn test_arch_installs_all_three_with_pacman_through_pkexec() {
    let step = qemu_step(&bare_arch());
    assert!(step.view.fixable);
    assert_eq!(step.view.reason.code, "setup.qemu.missing");
    assert_eq!(
        fixed_by(&step),
        [
            "pkexec",
            "sh",
            "-c",
            "pacman -S --needed --noconfirm qemu-system-x86 qemu-img qemu-ui-gtk edk2-ovmf"
        ]
    );
}

/// A desktop's apt lists can be old enough that the install 404s.
#[test]
fn test_debian_family_refreshes_apt_first() {
    let step = qemu_step(&debian());
    let line = fixed_by(&step)[3];
    assert!(
        line.starts_with("apt-get update && apt-get install -y"),
        "{line}"
    );
    assert!(line.contains("qemu-system-gui"), "{line}");
}

/// The case Debian and Ubuntu's base QEMU is: there, and no window.
#[test]
fn test_qemu_without_a_window_says_so_and_adds_it() {
    let host = Host {
        qemu: true,
        ..debian()
    };
    let step = qemu_step(&host);
    assert_eq!(step.view.reason.code, "setup.qemu.no_window");
    assert!(step.view.fixable);
}

#[test]
fn test_a_system_it_does_not_know_is_told_rather_than_guessed_at() {
    let host = Host {
        os_release: "ID=gentoo\n".into(),
        ..bare_arch()
    };
    let step = qemu_step(&host);
    assert!(!step.view.fixable);
    assert_eq!(step.view.reason.code, "setup.qemu.unknown_system");
}

#[test]
fn test_no_pkexec_gives_the_command_to_run_by_hand() {
    let host = Host {
        pkexec: false,
        ..bare_arch()
    };
    let step = qemu_step(&host);
    assert!(!step.view.fixable);
    assert_eq!(step.view.reason.code, "setup.qemu.by_hand");
    assert!(
        step.view.reason.values["command"]
            .as_str()
            .unwrap()
            .contains("pacman")
    );
    assert!(!acceleration_step(&host).view.fixable);
}

#[test]
fn test_windows_installs_qemu_with_winget_or_says_where_it_is() {
    let step = qemu_step(&windows());
    assert!(step.view.fixable);
    let argv = fixed_by(&step);
    assert_eq!(
        argv[..4],
        [
            "winget",
            "install",
            "--id",
            "SoftwareFreedomConservancy.QEMU"
        ]
    );
    assert!(argv.contains(&"--silent"));

    let without = Host {
        winget: false,
        ..windows()
    };
    let step = qemu_step(&without);
    assert!(!step.view.fixable);
    assert_eq!(step.view.reason.code, "setup.qemu.download");
}

#[test]
fn test_kvm_there_but_not_ours_joins_the_group_and_needs_a_new_login() {
    let step = acceleration_step(&bare_arch());
    assert!(step.view.fixable);
    assert_eq!(step.view.reason.code, "setup.acceleration.kvm_group");
    assert_eq!(
        fixed_by(&step),
        ["pkexec", "usermod", "-aG", "kvm", "alice"]
    );
}

/// Joined, but this session started before that: nothing to run, only to
/// log out and back in.
#[test]
fn test_already_in_the_group_only_needs_a_new_login() {
    let host = Host {
        in_kvm_group: true,
        ..bare_arch()
    };
    let step = acceleration_step(&host);
    assert!(!step.view.fixable);
    assert_eq!(step.view.reason.code, "setup.acceleration.relogin");
}

/// Off in the firmware: nothing BeatBox could run fixes that.
#[test]
fn test_no_kvm_device_is_not_offered_a_fix() {
    let mut host = bare_arch();
    host.kvm.device = false;
    let step = acceleration_step(&host);
    assert!(!step.view.fixable);
    assert_eq!(step.view.reason.code, "probe.kvm.missing");
}

/// Never handed to usermod as something it could read as an option.
#[test]
fn test_an_account_name_that_is_not_plain_is_not_used() {
    for name in ["-o", "Bob", "", "a b", "x;y"] {
        let host = Host {
            user: name.into(),
            ..bare_arch()
        };
        assert!(!acceleration_step(&host).view.fixable, "{name:?}");
    }
}

#[test]
fn test_windows_turns_the_hypervisor_platform_on_and_needs_a_restart() {
    let step = acceleration_step(&windows());
    assert!(step.view.fixable);
    assert_eq!(step.view.reason.code, "setup.acceleration.whpx");
    let argv = fixed_by(&step);
    assert_eq!(argv[0], "powershell");
    assert!(argv[4].contains("HypervisorPlatform") && argv[4].contains("RunAs"));

    let mut on = windows();
    on.kvm.whpx = true;
    assert!(acceleration_step(&on).view.ready);
}

#[test]
fn test_guest_tools_are_optional_and_say_how_big_they_are() {
    let step = guest_tools_step(false, "0.1.302", 31_642_483);
    assert!(step.view.optional && step.view.fixable);
    assert_eq!(step.view.reason.values["megabytes"], "31");
    assert!(guest_tools_step(true, "0.1.302", 1).view.ready);
}

#[test]
fn test_the_kvm_group_is_read_from_etc_group() {
    let groups = "wheel:x:998:bob\nkvm:x:992:alice,bob\nvideo:x:985:\n";
    assert!(in_group(groups, "kvm", "bob"));
    assert!(!in_group(groups, "kvm", "sam"));
    assert!(!in_group(groups, "video", "bob"));
}

fn answered(code: i32, stderr: &str) -> std::io::Result<Output> {
    Ok(Output {
        code,
        stdout: String::new(),
        stderr: stderr.into(),
    })
}

#[test]
fn test_what_a_fix_answered_is_read_as() {
    assert!(outcome(QEMU, false, answered(0, "")).0);
    let (ok, said) = outcome(QEMU, false, answered(126, ""));
    assert!(!ok);
    assert!(format!("{said:?}").contains("setup.dismissed"));
    let (ok, said) = outcome(
        QEMU,
        false,
        answered(1, "resolving...\nerror: target not found: qemu-ui-gtk\n"),
    );
    assert!(!ok);
    assert!(format!("{said:?}").contains("target not found"), "{said:?}");
    // DISM's "done, restart to finish" is a success.
    assert!(outcome(ACCELERATION, true, answered(3010, "")).0);
    assert!(!outcome(QEMU, true, answered(3010, "")).0);
    // UAC said No, or was never answered: as winget's exit code, or its text.
    for said in [
        outcome(QEMU, true, answered(0x8007_04C7_u32 as i32, "")).1,
        outcome(
            QEMU,
            true,
            answered(1, "0x800704c7 : The operation was canceled by the user."),
        )
        .1,
    ] {
        assert!(format!("{said:?}").contains("setup.declined"), "{said:?}");
    }
}

#[test]
fn test_a_fix_runs_its_command_as_a_job_and_says_what_comes_after() {
    let room = Room::new("setup-fix");
    let runner = Arc::new(Answers::default());
    let api = with(&room, bare_arch(), Arc::clone(&runner));
    let started = job(call(&api, "setup.fix", json!({"step": "acceleration"})));
    assert_eq!(started.kind, "setup");
    let done = settle(&api, &started.id, Duration::from_secs(5));
    assert_eq!(done.state, beatbox_api::jobs::DONE);
    assert_eq!(done.message, RELOGIN);
    assert_eq!(runner.argvs.lock().unwrap()[0][..2], ["pkexec", "usermod"]);
}

#[test]
fn test_a_step_that_is_fine_or_unknown_or_unfixable_is_refused() {
    let room = Room::new("setup-refused");
    let runner = Arc::new(Answers::default());
    let ready = Host {
        qemu: true,
        window: true,
        ..bare_arch()
    };
    let api = with(&room, ready, Arc::clone(&runner));
    assert_eq!(
        code(&call(&api, "setup.fix", json!({"step": "qemu"}))),
        "already"
    );
    assert_eq!(
        code(&call(&api, "setup.fix", json!({"step": "rm -rf /"}))),
        "bad-step"
    );

    let room = Room::new("setup-unfixable");
    let unknown = Host {
        os_release: "ID=gentoo\n".into(),
        ..bare_arch()
    };
    let api = with(&room, unknown, Arc::clone(&runner));
    assert_eq!(
        code(&call(&api, "setup.fix", json!({"step": "qemu"}))),
        "not-fixable"
    );
    assert!(
        runner.argvs.lock().unwrap().is_empty(),
        "nothing may have run"
    );
}
