//! Linux and Windows answers, tested on whichever system runs the suite:
//! Linux for real, Windows by handing in its answers where that's honest
//! (folders, arguments) and not where it isn't (`LockFileEx`,
//! `WinHvPlatform.dll`).

mod support;

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io;
use std::net::TcpListener;
use std::time::Duration;

use beatbox_isolation::endpoint::{
    chardev_argument, connect, option, prepare_with, qmp_argument, resolve,
};
use beatbox_isolation::host_os::{self, qemu_program_with};
use beatbox_isolation::machine::{
    MachineSpec, Qemu, accel_args, build_command, vnc_display_available,
};
use beatbox_isolation::runner::{Output, Runner};
use support::{Room, qemu};

struct VncHelp(&'static str);

impl Runner for VncHelp {
    fn run(&self, argv: &[String], _timeout: Option<Duration>) -> io::Result<Output> {
        assert_eq!(&argv[1..], ["-vnc", "help"]);
        Ok(Output {
            code: 1, // QEMU uses this status even when it prints valid help.
            stderr: self.0.into(),
            ..Output::default()
        })
    }
}

#[test]
fn a_headless_qemu_with_vnc_can_show_a_beatbox_window() {
    let options = "vnc options:\n  password-secret=<str>\n  websocket=<str>\n";
    assert!(vnc_display_available(&qemu(), &VncHelp(options)));
    assert!(!vnc_display_available(
        &qemu(),
        &VncHelp("vnc options:\n  password-secret=<str>\n"),
    ));
}

mod endpoint_tests {
    use super::*;

    #[test]
    fn on_linux_a_channel_is_its_own_socket_path() {
        let room = Room::new("endpoint");
        let path = room.join("monitor.sock");
        let at = prepare_with(&path, false).unwrap();
        assert_eq!(at, path.to_string_lossy());
        assert_eq!(qmp_argument(&at), format!("unix:{at},server=on,wait=off"));
        assert!(!path.exists(), "QEMU makes the socket, not BeatBox");
    }

    #[test]
    fn on_windows_a_channel_is_a_loopback_port_recorded_in_the_same_place() {
        let room = Room::new("endpoint");
        let path = room.join("monitor.sock");
        let at = prepare_with(&path, true).unwrap();
        let port = at
            .strip_prefix("tcp:127.0.0.1:")
            .expect("a loopback endpoint");
        assert!(port.parse::<u16>().is_ok());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), at);
        assert_eq!(resolve(&path), at);
        assert_eq!(qmp_argument(&at), format!("{at},server=on,wait=off"));
        assert_eq!(
            chardev_argument(&at, "qga0"),
            format!("socket,host=127.0.0.1,port={port},server=on,wait=off,id=qga0")
        );
    }

    /// QMP has no password. Whatever else changes, this does not listen on a
    /// network.
    #[test]
    fn never_anything_but_loopback() {
        let room = Room::new("endpoint");
        let at = prepare_with(&room.join("agent.sock"), true).unwrap();
        assert!(at.starts_with("tcp:127.0.0.1:"));
    }

    #[test]
    fn a_recorded_port_is_what_gets_connected_to() {
        let room = Room::new("endpoint");
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let record = room.join("monitor.sock");
        std::fs::write(
            &record,
            format!("tcp:127.0.0.1:{}", server.local_addr().unwrap().port()),
        )
        .unwrap();
        connect(&record, Duration::from_secs(2)).unwrap().shutdown();
    }

    #[test]
    fn a_comma_in_a_path_is_doubled_so_qemu_reads_it_as_one() {
        assert_eq!(option("/labs/Win10,x64.qcow2"), "/labs/Win10,,x64.qcow2");
    }
}

mod command_tests {
    use super::*;

    #[test]
    fn every_path_given_to_qemu_as_an_option_survives_a_comma() {
        let room = Room::new("comma");
        let disk = room.file("Lab, test/disk.qcow2", &[0u8; 512]);
        let capture = room.join("Lab, test/t.pcap");
        let spec = MachineSpec {
            name: "c".into(),
            image: disk.to_string_lossy().into_owned(),
            capture_path: capture.to_string_lossy().into_owned(),
            network: "isolated".into(),
            ..MachineSpec::default()
        };
        let command = build_command(&spec, &qemu(), "u").unwrap().join(" ");
        assert!(command.contains(&format!("file={},if=none", option(&disk.to_string_lossy()))));
        assert!(command.contains(&format!("file={}", option(&capture.to_string_lossy()))));
    }

    #[test]
    fn a_cabled_card_is_a_datagram_cable_and_nothing_slirp() {
        let room = Room::new("cable");
        let disk = room.file("disk.qcow2", &[0u8; 512]);
        let cable = beatbox_isolation::machine::Cable {
            switch: "/net,1/switch.sock".into(),
            port: "/net,1/a.sock".into(),
            mac: "02:bb:0a:0a:0a:0a".into(),
        };
        let spec = MachineSpec {
            name: "c".into(),
            image: disk.to_string_lossy().into_owned(),
            network: "attached".into(),
            cable: Some(cable),
            hostile: true,
            ..MachineSpec::default()
        };
        let command = build_command(&spec, &qemu(), "u").unwrap().join(" ");
        assert!(command.contains(
            "dgram,id=net0,local.type=unix,local.path=/net,,1/a.sock,remote.type=unix,remote.path=/net,,1/switch.sock"
        ));
        assert!(command.contains("e1000,netdev=net0,mac=02:bb:0a:0a:0a:0a"));
        assert!(!command.contains("user,"));
        let forwarding = MachineSpec {
            forwarded_ports: vec![(2222, 22)],
            ..spec.clone()
        };
        assert!(build_command(&forwarding, &qemu(), "u").is_err());
        let no_cable = MachineSpec {
            cable: None,
            ..spec
        };
        assert!(build_command(&no_cable, &qemu(), "u").is_err());
    }

    #[test]
    fn on_windows_with_whpx_a_guest_runs_on_it_and_falls_back_to_tcg() {
        let on = Qemu {
            whpx: true,
            ..qemu()
        };
        assert_eq!(
            accel_args(&on),
            ["-accel", "whpx,kernel-irqchip=off", "-accel", "tcg"]
        );
    }

    /// `-cpu host` and the hv-* enlightenments are KVM's; given to WHPX or
    /// TCG, QEMU refuses to start.
    #[test]
    fn no_kvm_cpu_and_no_hyperv_list_away_from_kvm() {
        assert_eq!(accel_args(&qemu()), ["-accel", "tcg"]);
    }

    #[test]
    fn a_cpu_model_can_be_chosen_away_from_kvm() {
        let chosen = Qemu {
            whpx: true,
            cpu: Some("Skylake-Client".into()),
            ..qemu()
        };
        let args = accel_args(&chosen);
        assert_eq!(args[args.len() - 2..], ["-cpu", "Skylake-Client"]);
    }
}

mod qemu_lookup_tests {
    use super::*;

    fn environment(pairs: &[(&str, OsString)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn a_folder_named_by_the_environment_comes_first() {
        let room = Room::new("qemudir");
        let suffix = if host_os::WINDOWS { ".exe" } else { "" };
        let program = room.file(&format!("qemu-system-x86_64{suffix}"), b"#!/bin/sh\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let env = environment(&[
            ("BEATBOX_QEMU_DIR", room.path().into()),
            ("PATH", std::env::var_os("PATH").unwrap_or_default()),
        ]);
        let found = qemu_program_with("qemu-system-x86_64", &env, host_os::WINDOWS).unwrap();
        assert_eq!(
            found.to_string_lossy().to_lowercase(),
            program.to_string_lossy().to_lowercase()
        );
    }

    /// The official installer puts QEMU in Program Files and does not add it
    /// to PATH.
    #[test]
    fn on_windows_program_files_is_looked_in_when_path_has_nothing() {
        let room = Room::new("programfiles");
        let exe = room.file("qemu/qemu-system-x86_64.exe", b"MZ");
        let env = environment(&[("ProgramW6432", room.path().into())]);
        assert_eq!(
            qemu_program_with("qemu-system-x86_64", &env, true),
            Some(exe)
        );
    }

    #[test]
    fn away_from_windows_path_is_the_only_place() {
        let env = environment(&[]);
        assert_eq!(qemu_program_with("qemu-system-x86_64", &env, false), None);
    }
}

mod host_tests {
    use super::*;

    #[test]
    fn memory_is_read_here() {
        if !(host_os::LINUX || host_os::WINDOWS) {
            return;
        }
        let (available, total) = host_os::memory_mb();
        let (available, total) = (available.unwrap(), total.unwrap());
        assert!(total > 0);
        assert!(available > 0);
        assert!(available <= total);
    }

    #[test]
    fn whpx_is_only_ever_claimed_on_windows() {
        if !host_os::WINDOWS {
            assert!(!host_os::whpx_available());
        }
    }

    #[test]
    fn one_lock_at_a_time() {
        let room = Room::new("lock");
        let open = || {
            OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(room.join(".owner"))
                .unwrap()
        };
        let (first, second) = (open(), open());
        assert!(host_os::lock_exclusively(&first));
        assert!(!host_os::lock_exclusively(&second));
    }
}

#[test]
fn meminfo_is_read_in_mebibytes() {
    let text = "MemTotal:       16001024 kB\nMemFree:  100 kB\nMemAvailable:    3276800 kB\n";
    assert_eq!(host_os::parse_meminfo(text), (Some(3200), Some(15626)));
    assert_eq!(host_os::parse_meminfo("nothing"), (None, None));
}

/// A backing file named with `\\?\` is one a Windows QEMU can't find.
#[test]
fn a_resolved_path_carries_no_verbatim_prefix() {
    use beatbox_isolation::host_os::plain;
    use std::path::PathBuf;
    assert_eq!(
        plain(PathBuf::from(r"\\?\C:\labs\disk.qcow2")),
        PathBuf::from(r"C:\labs\disk.qcow2")
    );
    assert_eq!(
        plain(PathBuf::from(r"\\?\UNC\nas\labs\disk.qcow2")),
        PathBuf::from(r"\\nas\labs\disk.qcow2")
    );
    assert_eq!(
        plain(PathBuf::from("/home/u/disk.qcow2")),
        PathBuf::from("/home/u/disk.qcow2")
    );
    let room = support::Room::new("canonical");
    let resolved = host_os::canonical(room.path()).unwrap();
    assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
}
