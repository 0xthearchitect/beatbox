//! Drive a real host sandbox the whole way through, and print what happened.
//!
//! The walk:
//!
//! ```text
//! Create -> Start -> Attach -> Execute -> Write -> Snapshot -> Modify -> Reset
//! ```
//!
//! It needs a disk. BeatBox never downloads one, so this does not either:
//!
//! ```text
//! cargo run -p beatbox-host --example host_walk -- --disk ~/vm/alma9.qcow2 --boot 60
//! cargo run -p beatbox-host --example host_walk            # an empty disk it makes itself
//! ```
//!
//! With no `--disk` it boots an empty 64M qcow2: nothing inside answers, but
//! everything outside is real (overlay, monitor, pause, snapshots, reset, and
//! the refusals without an agent). A disk with `qemu-guest-agent` brings the
//! inside half alive too.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{
    ActionOutcome, Data, Recorder, Resources, RuntimeParts, SandboxConfig, SandboxRegistry,
    SandboxRuntime, data,
};
use beatbox_host::HostSandbox;

const GREY: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const OFF: &str = "\x1b[0m";
const MARK: &str = "/tmp/beatbox-was-here";

fn step(title: &str) {
    println!("\n{BOLD}{title}{OFF}");
}

fn say(outcome: &ActionOutcome) -> String {
    if outcome.ok {
        format!("ok {}", serde_json::Value::Object(outcome.data.clone()))
    } else {
        format!(
            "no {}",
            outcome
                .reason
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default()
        )
    }
}

fn option(name: &str) -> Option<String> {
    let arguments: Vec<String> = std::env::args().collect();
    arguments
        .iter()
        .position(|argument| argument == name)
        .and_then(|at| arguments.get(at + 1).cloned())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let network = option("--network").unwrap_or_else(|| "none".into());
    let boot: f64 = option("--boot").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let room = std::env::temp_dir().join(format!("beatbox-host-walk-{}", std::process::id()));
    std::fs::create_dir_all(&room)?;
    let disk = match option("--disk") {
        Some(disk) => disk,
        None => {
            let empty = room.join("empty.qcow2");
            Command::new("qemu-img")
                .args(["create", "-f", "qcow2"])
                .arg(&empty)
                .arg("64M")
                .output()?;
            println!(
                "{GREY}  no --disk given, so: an empty {}{OFF}",
                empty.display()
            );
            empty.to_string_lossy().into_owned()
        }
    };

    let mut registry = SandboxRegistry::default();
    registry.register(HostSandbox::spec())?;
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let log = Recorder::default();
    runtime.events().subscribe("*", |event| {
        println!(
            "{GREY}  · {:20} {}{OFF}",
            event.name(),
            serde_json::Value::Object(event.data().clone())
        );
    })?;
    runtime.events().subscribe("*", log.sink())?;

    step("What can this runtime build?");
    for entry in runtime.registry().types() {
        println!(
            "  {} v{}, isolates at '{}'",
            entry.name, entry.version, entry.isolation
        );
    }

    step("Create");
    let config = SandboxConfig::new("host")?
        .named("target")
        .with_resources(Resources::new([(CPU, 2.0), (MEMORY, 2048.0)])?)
        .with_settings(data!({
            "disk": disk,
            "os": {"family": "linux", "distribution": "almalinux", "version": "9"},
            "network": network,
            "workspace": room.join("sbx").to_string_lossy(),
        }));
    let guest = runtime.create(config)?;
    let facts = guest.state().facts;
    println!("  {guest}");
    println!("  it can: {}", guest.capabilities().names().join(", "));
    println!("  its own disk: {}", facts["overlay"]);
    println!("  the disk you gave it is untouched: {}", facts["disk"]);

    step("Start");
    guest.start()?;
    let facts = guest.state().facts;
    println!(
        "  {guest}  QEMU says {}, accelerated={}",
        facts["status"], facts["accelerated"]
    );
    if boot > 0.0 {
        step(&format!("Waiting {boot}s for something inside to answer"));
        let deadline = Instant::now() + Duration::from_secs_f64(boot);
        while Instant::now() < deadline && guest.refresh()?.facts["agent"] != true {
            std::thread::sleep(Duration::from_secs(1));
        }
        println!("  agent: {}", guest.state().facts["agent"]);
    }

    step("Ask the guest to do something");
    let steps: [(&str, Data); 5] = [
        (
            "process.execute",
            data!({"command": "/bin/uname", "arguments": ["-a"]}),
        ),
        ("process.list", data!({})),
        ("network.interfaces", data!({})),
        (
            "filesystem.write",
            data!({"path": MARK, "content": "beatbox was here\n"}),
        ),
        ("filesystem.read", data!({"path": MARK})),
    ];
    for (capability, arguments) in steps {
        println!(
            "  {capability:20} -> {}",
            say(&guest.act(capability, arguments)?)
        );
    }

    step("Freeze it, and let it go again");
    guest.pause()?;
    println!("  {guest}  QEMU says {}", guest.refresh()?.facts["status"]);
    guest.resume()?;
    println!("  {guest}  QEMU says {}", guest.refresh()?.facts["status"]);

    step("Snapshot, change something, and go back");
    let act = |capability: &str, arguments: Data| guest.act(capability, arguments).map(|o| say(&o));
    println!(
        "  snapshot.create      -> {}",
        act("snapshot.create", data!({"name": "before_attack"}))?
    );
    println!(
        "  snapshot.list        -> {}",
        act("snapshot.list", data!({}))?
    );
    println!(
        "  process.execute rm   -> {}",
        act(
            "process.execute",
            data!({"command": "/bin/rm", "arguments": ["-f", MARK]})
        )?
    );
    println!(
        "  filesystem.read      -> {}",
        act("filesystem.read", data!({"path": MARK}))?
    );
    println!(
        "  snapshot.restore     -> {}",
        act("snapshot.restore", data!({"name": "before_attack"}))?
    );
    println!(
        "  filesystem.read      -> {}",
        act("filesystem.read", data!({"path": MARK}))?
    );

    step("Reset: back to the disk it was created with");
    guest.reset()?;
    println!("  {guest}");
    println!("  snapshots now: {}", act("snapshot.list", data!({}))?);

    step("Destroy");
    let sandbox_room = Path::new(guest.state().facts["overlay"].as_str().unwrap_or_default())
        .parent()
        .map(Path::to_path_buf);
    guest.destroy()?;
    println!("  {guest}");
    println!(
        "  its disk is gone: {}",
        sandbox_room.is_some_and(|r| !r.exists())
    );
    println!(
        "  the disk you gave it is still there: {}",
        Path::new(&disk).exists()
    );
    println!(
        "  {} events, {} of them from QEMU itself",
        runtime.events().published(),
        log.matching("guest.*").len()
    );
    let _ = std::fs::remove_dir_all(&room);
    Ok(())
}
