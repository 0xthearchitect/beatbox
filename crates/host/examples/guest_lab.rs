//! Drive one guest by hand, a command at a time.
//!
//! ```text
//! guest_lab start <disk> <room> [memory-mb] [cpus] [none|nat]   an overlay of <disk> in <room>, booted, headless
//! guest_lab screen <room> <file.png>                 what is on its screen
//! guest_lab keys <room> <key>...                     press keys: tab, ret, ctrl-alt-delete, a, ...
//! guest_lab agent <room> [seconds]                   wait for the agent inside to answer
//! guest_lab put <room> <local> <remote>              copy a file in, through the agent
//! guest_lab run <room> <seconds> <program> [arg]...  run something inside and print what it said
//! guest_lab stop <room>                              ask QEMU to go
//! ```
//!
//! `<disk>` is only read: the guest runs on its own overlay in `<room>`.
//! QEMU outlives this program; each command reconnects over the sockets in
//! `<room>`. Keep `<room>` off tmpfs `/tmp`, and its path short (a UNIX
//! socket path is under 108 bytes).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use beatbox_isolation::endpoint;
use beatbox_isolation::machine::{MachineSpec, Qemu, create_overlay, start_machine};
use beatbox_isolation::qmp::{GuestAgent, Monitor};
use beatbox_isolation::runner::SystemRunner;
use serde_json::{Value, json};

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn monitor(room: &Path) -> Result<Monitor, Box<dyn std::error::Error>> {
    Ok(Monitor::connect(
        &room.join("monitor.sock"),
        Duration::from_secs(5),
    )?)
}

fn agent(room: &Path, timeout: Duration) -> Option<GuestAgent> {
    let mut agent = GuestAgent::connect(&room.join("agent.sock"), timeout).ok()?;
    agent.sync(timeout).then_some(agent)
}

fn start(disk: &str, room: &Path, memory: i64, cpus: i64, network: &str) -> Outcome {
    std::fs::create_dir_all(room)?;
    let overlay = room.join("disk.qcow2");
    let qemu = Qemu::here();
    if !overlay.exists() {
        create_overlay(disk, &overlay.to_string_lossy(), &qemu, &SystemRunner)?;
    }
    let spec = MachineSpec {
        name: "guest-lab".into(),
        image: overlay.to_string_lossy().into_owned(),
        memory_mb: memory,
        cpus,
        headless: true,
        disposable: false,
        network: if network.is_empty() {
            "none".into()
        } else {
            network.into()
        },
        monitor_socket: endpoint::prepare(&room.join("monitor.sock"))?,
        agent_socket: endpoint::prepare(&room.join("agent.sock"))?,
        log_path: room.join("qemu.log").to_string_lossy().into_owned(),
        ..MachineSpec::default()
    };
    let machine = start_machine(&spec, &qemu, &beatbox_isolation::uuid4())?;
    println!(
        "started: pid {}, accelerated={}",
        machine.child.id(),
        machine.accelerated
    );
    Ok(())
}

fn main() -> Outcome {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let word = |at: usize| arguments.get(at).cloned().unwrap_or_default();
    let room = PathBuf::from(word(1));
    match word(0).as_str() {
        "start" => start(
            &word(1),
            Path::new(&word(2)),
            word(3).parse().unwrap_or(2048),
            word(4).parse().unwrap_or(2),
            &word(5),
        )?,
        "screen" => {
            monitor(&room)?.execute(
                "screendump",
                Some(json!({"filename": word(2), "format": "png"})),
            )?;
            println!("{}", word(2));
        }
        "keys" => {
            let mut monitor = monitor(&room)?;
            for combination in &arguments[2..] {
                let keys: Vec<Value> = combination
                    .split('-')
                    .map(|key| json!({"type": "qcode", "data": key}))
                    .collect();
                monitor.execute("send-key", Some(json!({"keys": keys})))?;
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        "agent" => {
            let seconds: u64 = word(2).parse().unwrap_or(300);
            let deadline = Instant::now() + Duration::from_secs(seconds);
            loop {
                if let Some(mut agent) = agent(&room, Duration::from_secs(2)) {
                    println!(
                        "agent: {}",
                        Value::Object(agent.info()?)
                            .get("version")
                            .cloned()
                            .unwrap_or_default()
                    );
                    break;
                }
                if Instant::now() > deadline {
                    return Err("no agent answered".into());
                }
                std::thread::sleep(Duration::from_secs(3));
            }
        }
        "put" => {
            let bytes = std::fs::read(word(2))?;
            let mut agent = agent(&room, Duration::from_secs(5)).ok_or("no agent")?;
            let started = Instant::now();
            let written = agent.write_bytes(&word(3), &bytes)?;
            println!("{written} bytes in {:.1}s", started.elapsed().as_secs_f64());
        }
        "run" => {
            let seconds: u64 = word(2).parse().unwrap_or(60);
            let rest: Vec<Value> = arguments[4..].iter().map(|a| json!(a)).collect();
            let mut agent = agent(&room, Duration::from_secs(5)).ok_or("no agent")?;
            let result = agent.run(&word(3), &rest, Some(Duration::from_secs(seconds)))?;
            print!("{}", result["stdout"].as_str().unwrap_or_default());
            eprint!("{}", result["stderr"].as_str().unwrap_or_default());
            println!("exit: {}", result["exit_code"]);
        }
        "stop" => monitor(&room)?.quit()?,
        _ => {
            eprintln!("see the top of crates/host/examples/guest_lab.rs");
            std::process::exit(2);
        }
    }
    Ok(())
}
