//! The inside half, against a guest that really runs `qemu-guest-agent`.
//!
//! Run inside, write and read a file, read interfaces, take a save point,
//! remove the file, restore, and find it back. Only runs when pointed at a
//! disk with an agent, and never writes that disk (it gets an overlay).
//!
//! ```text
//! BEATBOX_AGENT_DISK=~/vm/alma9.qcow2 cargo test -p beatbox-host --test real_agent -- --nocapture
//! BEATBOX_AGENT_DISK=... BEATBOX_AGENT_OS=windows cargo test -p beatbox-host --test real_agent -- --nocapture
//! ```

mod support;

use std::time::{Duration, Instant};

use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{Lifecycle, Resources, SandboxConfig, data};
use serde_json::Value;
use support::{Room, build, one_guest_at_a_time};

#[test]
fn a_guest_with_an_agent_does_what_it_is_asked_inside() {
    let Ok(disk) = std::env::var("BEATBOX_AGENT_DISK") else {
        eprintln!("skipped: BEATBOX_AGENT_DISK names no disk with an agent on it");
        return;
    };
    needs_qemu!();
    let windows = std::env::var("BEATBOX_AGENT_OS").is_ok_and(|os| os == "windows");
    let _one = one_guest_at_a_time();
    let room = Room::new("agent");
    let (runtime, log) = build();
    let family = if windows { "windows" } else { "linux" };
    let guest = runtime
        .create(
            SandboxConfig::new("host")
                .unwrap()
                .named("agent-walk")
                .with_resources(Resources::new([(CPU, 2.0), (MEMORY, 2048.0)]).unwrap())
                .with_settings(
                    data!({"disk": disk, "workspace": room.text("sbx"), "os": {"family": family}}),
                ),
        )
        .unwrap();
    guest.start().unwrap();

    let deadline = Instant::now() + Duration::from_secs(300);
    while guest.refresh().unwrap().facts["agent"] != true {
        assert!(
            Instant::now() < deadline,
            "no agent answered within five minutes"
        );
        std::thread::sleep(Duration::from_secs(3));
    }
    eprintln!(
        "agent answered after booting; addresses {}",
        guest.state().facts["addresses"]
    );

    let (program, arguments, mark) = if windows {
        (
            "cmd.exe",
            vec!["/c", "ver"],
            r"C:\Users\Public\beatbox-was-here.txt",
        )
    } else {
        ("/bin/uname", vec!["-a"], "/tmp/beatbox-was-here")
    };
    let ran = guest
        .act(
            "process.execute",
            data!({"command": program, "arguments": arguments}),
        )
        .unwrap();
    assert!(ran.ok, "{:?}", ran.reason);
    assert_eq!(ran.data["exit_code"], 0);
    let said = ran.data["stdout"].as_str().unwrap_or_default().to_owned();
    eprintln!("{program} said: {}", said.trim());
    assert!(!said.trim().is_empty());
    assert!(!log.matching("process.exited").is_empty());

    let wrote = guest
        .act(
            "filesystem.write",
            data!({"path": mark, "content": "beatbox was here\n"}),
        )
        .unwrap();
    assert!(wrote.ok, "{:?}", wrote.reason);
    let read = guest.act("filesystem.read", data!({"path": mark})).unwrap();
    assert_eq!(read.data["content"], "beatbox was here\n");

    let interfaces = guest.act("network.interfaces", data!({})).unwrap();
    assert!(interfaces.ok);
    assert!(
        interfaces.data["interfaces"]
            .as_array()
            .is_some_and(|list| !list.is_empty())
    );

    assert!(
        guest
            .act("snapshot.create", data!({"name": "with the mark"}))
            .unwrap()
            .ok
    );
    let (remove, remove_arguments) = if windows {
        ("cmd.exe", vec!["/c", "del", mark])
    } else {
        ("/bin/rm", vec!["-f", mark])
    };
    assert!(
        guest
            .act(
                "process.execute",
                data!({"command": remove, "arguments": remove_arguments})
            )
            .unwrap()
            .ok
    );
    let gone = guest.act("filesystem.read", data!({"path": mark})).unwrap();
    assert_eq!(
        gone.reason.map(|why| why.code),
        Some("engine.no_such_file".into())
    );

    assert!(
        guest
            .act("snapshot.restore", data!({"name": "with the mark"}))
            .unwrap()
            .ok
    );
    // The agent inside was restored too, from a moment before this channel
    // asked anything; give it a look to be found again.
    let deadline = Instant::now() + Duration::from_secs(60);
    let back = loop {
        let read = guest.act("filesystem.read", data!({"path": mark})).unwrap();
        if read.ok || Instant::now() > deadline {
            break read;
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    assert_eq!(
        back.data.get("content").cloned(),
        Some(Value::from("beatbox was here\n"))
    );

    assert_eq!(guest.stop().unwrap().lifecycle, Lifecycle::Stopped);
    guest.destroy().unwrap();
}
