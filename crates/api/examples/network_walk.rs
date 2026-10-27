//! Two real guests on one network sandbox, driven only through the engine.
//!
//! ```text
//! create network -> create two hosts -> attach both -> start
//!   -> A pings B, the network sees it -> DENY, it stops -> ALLOW, it is back
//!   -> 100 ms + 5% loss, and it shows -> reset, and it is gone
//! ```
//!
//! It needs a Linux disk with qemu-guest-agent on it, and BeatBox never
//! fetches one. The disk is never written: each guest gets an overlay of its
//! own in a room under `~/.beatbox/walks/`, deleted at the end.
//!
//! ```bash
//! cargo run -p beatbox-api --example network_walk -- --disk ~/.beatbox/disks/ubuntu.qcow2
//! ```

use std::time::{Duration, Instant};

use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{
    ActionOutcome, Data, Resources, RuntimeParts, Sandbox, SandboxConfig, SandboxRegistry,
    SandboxRuntime, data,
};
use beatbox_host::HostSandbox;
use beatbox_network::NetworkSandbox;

const GREY: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const OFF: &str = "\x1b[0m";

fn step(title: &str) {
    println!("\n{BOLD}{title}{OFF}");
}

fn option(name: &str) -> Option<String> {
    let arguments: Vec<String> = std::env::args().collect();
    arguments
        .iter()
        .position(|argument| argument == name)
        .and_then(|at| arguments.get(at + 1).cloned())
}

fn ok(outcome: ActionOutcome) -> Result<Data, String> {
    if outcome.ok {
        Ok(outcome.data)
    } else {
        Err(format!(
            "{} refused: {:?}",
            outcome.capability(),
            outcome.reason
        ))
    }
}

/// `ping` from inside `guest`: how many came back, and the summary line.
fn ping(
    guest: &Sandbox,
    address: &str,
    count: u32,
) -> Result<(u32, String), Box<dyn std::error::Error>> {
    let count_text = count.to_string();
    let result = ok(guest.act(
        "process.execute",
        data!({
            "command": "/usr/bin/ping",
            "arguments": ["-c", count_text, "-i", "0.2", "-W", "1", address],
            "timeout": 60,
        }),
    )?)?;
    let stdout = result["stdout"].as_str().unwrap_or_default().to_owned();
    let received = stdout
        .lines()
        .find(|line| line.contains("received"))
        .and_then(|line| line.split(", ").nth(1))
        .and_then(|part| part.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let summary = stdout
        .lines()
        .filter(|line| line.contains("received") || line.starts_with("rtt"))
        .collect::<Vec<_>>()
        .join(" | ");
    Ok((received, summary))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(disk) = option("--disk") else {
        eprintln!(
            "usage: network_walk --disk <a Linux disk with qemu-guest-agent> [--memory MB] [--boot SECONDS]"
        );
        std::process::exit(2);
    };
    let memory: f64 = option("--memory")
        .and_then(|m| m.parse().ok())
        .unwrap_or(1536.0);
    let boot: u64 = option("--boot").and_then(|b| b.parse().ok()).unwrap_or(240);
    let home = std::env::var("HOME")?;
    let room = std::path::Path::new(&home)
        .join(".beatbox/walks")
        .join(format!("network-{}", std::process::id()));
    std::fs::create_dir_all(&room)?;

    let mut registry = SandboxRegistry::default();
    registry.register(HostSandbox::spec())?;
    registry.register(NetworkSandbox::spec())?;
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    runtime.events().subscribe("*", |event| {
        let name = event.name();
        if !name.starts_with("process.") {
            println!(
                "{GREY}  · {name:20} {}{OFF}",
                serde_json::Value::Object(event.data().clone())
            );
        }
    })?;

    let outcome = walk(&runtime, &disk, memory, boot, &room);
    step("Tear down");
    runtime.destroy_all();
    let _ = std::fs::remove_dir_all(&room);
    println!("  nothing left in {}", room.display());
    outcome
}

fn walk(
    runtime: &SandboxRuntime,
    disk: &str,
    memory: f64,
    boot: u64,
    room: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    step("1. Create the network");
    let lab = runtime.create(
        SandboxConfig::new("network")?
            .named("lab")
            .with_settings(data!({
                "cidr": "10.10.10.0/24",
                "workspace": room.join("net").to_string_lossy(),
            })),
    )?;
    lab.start()?;
    println!("  {lab}");

    step("2 & 3. Create two hosts, and attach them");
    let mut hosts = Vec::new();
    for (name, address) in [("host-a", "10.10.10.10"), ("host-b", "10.10.10.20")] {
        let cable = ok(lab.act("network.attach", data!({"node": name, "address": address}))?)?;
        let host = runtime.create(
            SandboxConfig::new("host")?
                .named(name)
                .with_resources(Resources::new([(CPU, 1.0), (MEMORY, memory)])?)
                .with_settings(data!({
                    "disk": disk,
                    "network": cable,
                    "workspace": room.join(name).to_string_lossy(),
                })),
        )?;
        println!("  {host}  cabled as {}", cable["address"]);
        hosts.push(host);
    }

    step("4. Start");
    for host in &hosts {
        host.start()?;
    }
    let deadline = Instant::now() + Duration::from_secs(boot);
    for host in &hosts {
        while host.refresh()?.facts["agent"] != true {
            if Instant::now() > deadline {
                return Err(format!("{host}: no guest agent after {boot}s").into());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    // The agent answers before NetworkManager has a lease; wait for the one
    // address the network will ever give each of them.
    for (host, address) in hosts.iter().zip(["10.10.10.10", "10.10.10.20"]) {
        while !serde_json::Value::Object(host.act("network.interfaces", Data::new())?.data)
            .to_string()
            .contains(address)
        {
            if Instant::now() > deadline {
                return Err(format!("{host}: never got {address}").into());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        println!("  {host} has {address}");
    }
    let (a, b) = (&hosts[0], &hosts[1]);

    step("5 & 6. A pings B, and the network sees it");
    let (got, summary) = ping(a, "10.10.10.20", 5)?;
    println!("  {summary}");
    assert!(got > 0, "A could not reach B");
    let flows = ok(lab.act("network.flows", Data::new())?)?;
    for flow in flows["flows"].as_array().into_iter().flatten().take(4) {
        println!("  flow {flow}");
    }

    step("7 & 8. DENY host-a -> host-b");
    let rule = ok(lab.act(
        "firewall.add",
        data!({"action": "deny", "source": "host-a", "destination": "host-b"}),
    )?)?;
    let (got, summary) = ping(a, "10.10.10.20", 3)?;
    println!("  {summary}");
    assert_eq!(got, 0, "a denied ping got through");

    step("9 & 10. ALLOW again");
    ok(lab.act("firewall.remove", data!({"id": rule["id"]}))?)?;
    ok(lab.act(
        "firewall.add",
        data!({"action": "allow", "source": "host-a", "destination": "host-b"}),
    )?)?;
    let (got, summary) = ping(a, "10.10.10.20", 3)?;
    println!("  {summary}");
    assert!(got > 0, "an allowed ping did not get through");

    step("11 & 12. 100 ms and 5% loss");
    ok(lab.act(
        "traffic.configure",
        data!({"latency_ms": 100, "loss_percent": 5}),
    )?)?;
    let (_, summary) = ping(a, "10.10.10.20", 20)?;
    println!("  {summary}");
    println!("  counters {}", lab.refresh()?.facts["counters"]);

    step("13 & 14. Reset the network: back to how it was made");
    lab.reset()?;
    lab.start()?;
    let facts = lab.refresh()?.facts;
    println!(
        "  rules {}  traffic {}",
        facts["firewall"]["rules"], facts["traffic"]
    );
    let (got, summary) = ping(a, "10.10.10.20", 5)?;
    println!("  {summary}");
    assert!(got > 0, "after the reset A could not reach B");
    let _ = b;
    Ok(())
}
