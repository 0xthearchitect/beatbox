//! A real QEMU on a network's cable, on whichever wire this run uses (UNIX
//! sockets, or UDP with `BEATBOX_NETWORK_WIRE=udp`). No disk: the card's
//! own boot ROM asks the network's DHCP for an address, which is the first
//! thing every guest does. Skipped where QEMU is not installed.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use beatbox_engine::{RuntimeParts, SandboxConfig, SandboxRegistry, SandboxRuntime, data};
use beatbox_isolation::host_os::qemu_program;
use beatbox_isolation::machine::{Cable, cable_netdev};
use beatbox_network::NetworkSandbox;

/// Killed when the test ends, passed or not.
struct Guest(Child);

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_real_qemu_card_gets_a_lease_over_the_cable() {
    let Some(qemu) = qemu_program("qemu-system-x86_64") else {
        eprintln!("no qemu-system-x86_64: skipped");
        return;
    };
    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    // Short: a UNIX socket's path has to fit in 107 bytes.
    let tmp = if cfg!(unix) {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let room = tmp.join(format!("bbq-{}", std::process::id()));
    let config = SandboxConfig::new("network")
        .unwrap()
        .named("lab")
        .with_settings(data!({"workspace": room.to_string_lossy()}));
    let lab = runtime.create(config).unwrap();
    lab.start().unwrap();
    let answer = lab.act("network.attach", data!({"node": "vm"})).unwrap();
    assert!(answer.ok, "{:?}", answer.reason);
    let text = |key: &str| answer.data[key].as_str().unwrap().to_owned();
    let cable = Cable {
        switch: text("switch"),
        port: text("port"),
        mac: text("mac"),
    };

    let _guest = Guest(
        Command::new(qemu)
            .args(["-machine", "q35", "-accel", "tcg", "-m", "64"])
            .args(["-nodefaults", "-display", "none", "-boot", "n"])
            .args(["-netdev", &cable_netdev(&cable)])
            .args(["-device", &format!("e1000,netdev=net0,mac={}", cable.mac)])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // iPXE under plain emulation asks within a few seconds.
    let deadline = Instant::now() + Duration::from_secs(90);
    let leased = loop {
        let facts = lab.refresh().unwrap().facts;
        let vm = facts["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node"] == "vm")
            .cloned()
            .unwrap();
        if !vm["lease_ms"].is_null() || Instant::now() > deadline {
            break !vm["lease_ms"].is_null();
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert!(
        leased,
        "no DHCP lease for the QEMU card over {}",
        cable.port
    );
    lab.stop().unwrap();
    lab.destroy().unwrap();
}
