//! Issue #52's proof with a real guest: a machine that asks DHCP for itself,
//! on a network whose router was told how to hand out addresses.
//!
//! ```text
//! network with a pool, a lease, a DNS and a domain -> one real guest, cabled
//!   -> it takes an address from the pool, and the capture of its cable shows
//!      the router's answer (lease, DNS, domain) -> the pool moves away from
//!      it, and it takes a new address when it renews -> it asks for names
//! ```
//!
//! No guest agent is needed: everything is read from the network's side (its
//! facts, and a capture of the guest's cable). The disk is never written:
//! the guest gets an overlay of its own under `~/.beatbox/walks/`.
//!
//! ```bash
//! cargo run -p beatbox-api --example dhcp_walk -- --disk ~/.beatbox/disks/ubuntu.qcow2
//! ```

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{
    ActionOutcome, Data, Resources, RuntimeParts, Sandbox, SandboxConfig, SandboxRegistry,
    SandboxRuntime, data,
};
use beatbox_host::HostSandbox;
use beatbox_network::NetworkSandbox;

fn step(title: &str) {
    println!("\n\x1b[1m{title}\x1b[0m");
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

/// What the guest holds, from the network's side: its address and when it
/// was last given it.
fn held(lab: &Sandbox, node: &str) -> Result<(Ipv4Addr, u64), Box<dyn std::error::Error>> {
    let facts = lab.refresh()?.facts;
    let row = facts["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["node"] == node)
        .ok_or("the guest is not cabled")?;
    Ok((
        row["address"].as_str().unwrap_or_default().parse()?,
        row["lease_ms"].as_u64().unwrap_or(0),
    ))
}

/// One DHCP answer: its type, the address given, the lease, the DNS and the domain.
type Answer = (u8, Ipv4Addr, Option<u32>, Option<Ipv4Addr>, Option<String>);

/// The DHCP answers in a pcap (classic, Ethernet), with options 51 (lease),
/// 6 (DNS) and 15 (domain).
fn answers(pcap: &[u8]) -> Vec<Answer> {
    let mut found = Vec::new();
    let mut at = 24;
    while at + 16 <= pcap.len() {
        let size = u32::from_le_bytes(pcap[at + 8..at + 12].try_into().unwrap()) as usize;
        let frame = &pcap[(at + 16).min(pcap.len())..(at + 16 + size).min(pcap.len())];
        at += 16 + size;
        // Ethernet, IPv4, UDP from port 67 to 68, a BOOTP reply with its cookie.
        if frame.len() < 14 + 20 + 8 + 240 || frame[12..14] != [8, 0] || frame[23] != 17 {
            continue;
        }
        let ip = 14 + usize::from(frame[14] & 0x0f) * 4;
        if frame[ip..ip + 4] != [0, 67, 0, 68] {
            continue;
        }
        let bootp = &frame[ip + 8..];
        if bootp[0] != 2 || bootp[236..240] != [99, 130, 83, 99] {
            continue;
        }
        let (mut kind, mut lease, mut dns, mut domain) = (0, None, None, None);
        let mut options = &bootp[240..];
        while let [code, rest @ ..] = options {
            if *code == 255 {
                break;
            }
            if *code == 0 {
                options = rest;
                continue;
            }
            let Some((&length, rest)) = rest.split_first() else {
                break;
            };
            let Some(value) = rest.get(..usize::from(length)) else {
                break;
            };
            match (*code, value) {
                (53, [k]) => kind = *k,
                (51, [a, b, c, d]) => lease = Some(u32::from_be_bytes([*a, *b, *c, *d])),
                (6, [a, b, c, d, ..]) => dns = Some(Ipv4Addr::new(*a, *b, *c, *d)),
                (15, text) => domain = Some(String::from_utf8_lossy(text).into_owned()),
                _ => {}
            }
            options = &rest[usize::from(length)..];
        }
        let given = Ipv4Addr::new(bootp[16], bootp[17], bootp[18], bootp[19]);
        found.push((kind, given, lease, dns, domain));
    }
    found
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(disk) = option("--disk") else {
        eprintln!("usage: dhcp_walk --disk <a Linux disk> [--memory MB] [--boot SECONDS]");
        std::process::exit(2);
    };
    let memory: f64 = option("--memory")
        .and_then(|m| m.parse().ok())
        .unwrap_or(1536.0);
    let boot: u64 = option("--boot").and_then(|b| b.parse().ok()).unwrap_or(240);
    let home = std::env::var("HOME")?;
    let room = std::path::Path::new(&home)
        .join(".beatbox/walks")
        .join(format!("dhcp-{}", std::process::id()));
    std::fs::create_dir_all(&room)?;

    let mut registry = SandboxRegistry::default();
    registry.register(HostSandbox::spec())?;
    registry.register(NetworkSandbox::spec())?;
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
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
    step("1. A network that hands out .60 to .89 for two minutes, DNS .53, domain lab");
    let lab = runtime.create(
        SandboxConfig::new("network")?
            .named("lab")
            .with_settings(data!({
                "cidr": "10.10.10.0/24",
                "workspace": room.join("net").to_string_lossy(),
            })),
    )?;
    lab.start()?;
    ok(lab.act("dns.domain", data!({"domain": "lab"}))?)?;
    ok(lab.act(
        "dhcp.configure",
        data!({"vlan": 1, "first": 60, "last": 89, "lease": 120, "dns": "10.10.10.53"}),
    )?)?;
    ok(lab.act(
        "dns.add",
        data!({"name": "portal.lab", "address": "10.10.10.200"}),
    )?)?;

    step("2. One real guest, cabled, with a capture on its cable");
    // Cabled the way a virtual machine is, then moved into the pool: the
    // address `network.attach` gives is .10, outside it.
    let cable = ok(lab.act("network.attach", data!({"node": "guest"}))?)?;
    ok(lab.act("node.reserve", data!({"node": "guest", "on": false}))?)?;
    let started = ok(lab.act("capture.start", data!({"node": "guest", "keep": "all"}))?)?;
    println!("  cabled as {}", cable["address"]);
    let guest = runtime.create(
        SandboxConfig::new("host")?
            .named("guest")
            .with_resources(Resources::new([(CPU, 1.0), (MEMORY, memory)])?)
            .with_settings(data!({
                "disk": disk,
                "network": cable,
                "workspace": room.join("guest").to_string_lossy(),
            })),
    )?;
    guest.start()?;
    let (address, _) = held(&lab, "guest")?;
    assert!(
        (60..=89).contains(&address.octets()[3]),
        "the pool, not {address}"
    );
    println!("  the network put it at {address}, inside the pool");

    step("3. It asks DHCP for itself: its first lease");
    let deadline = Instant::now() + Duration::from_secs(boot);
    let first = loop {
        let (_, at) = held(&lab, "guest")?;
        if at > 0 {
            break at;
        }
        if Instant::now() > deadline {
            return Err(format!("no lease after {boot}s").into());
        }
        std::thread::sleep(Duration::from_secs(2));
    };
    println!("  leased at {first} ms since the epoch");

    step("4. What the router told it, from the capture of its cable");
    let id = started["id"].as_str().ok_or("no capture id")?.to_owned();
    let path = room.join("net/captures").join(format!("{id}.pcap"));
    std::thread::sleep(Duration::from_secs(3));
    let seen = answers(&std::fs::read(&path)?);
    for (kind, given, lease, dns, domain) in &seen {
        println!("  type {kind}: {given} lease {lease:?} dns {dns:?} domain {domain:?}");
    }
    let ack = seen
        .iter()
        .find(|(kind, ..)| *kind == 5)
        .ok_or("no DHCP ACK on the cable")?;
    assert_eq!(ack.1, address, "it was given the address the network chose");
    assert_eq!(ack.2, Some(120), "a lease of two minutes");
    assert_eq!(ack.3, Some(Ipv4Addr::new(10, 10, 10, 53)), "the DNS told");
    assert_eq!(ack.4.as_deref(), Some("lab"), "the domain told");

    step("5. The pool moves away: .70 to .79");
    ok(lab.act(
        "dhcp.configure",
        data!({"vlan": 1, "first": 70, "last": 79}),
    )?)?;
    let (moved, _) = held(&lab, "guest")?;
    assert!(
        (70..=79).contains(&moved.octets()[3]),
        "the network moved it into the pool"
    );
    println!(
        "  the network moved it to {moved}; the guest learns when it renews (within a minute)"
    );
    let renew = Instant::now() + Duration::from_secs(150);
    let renewed = loop {
        let (_, at) = held(&lab, "guest")?;
        if at > first {
            break at;
        }
        if Instant::now() > renew {
            return Err("it never renewed".into());
        }
        std::thread::sleep(Duration::from_secs(3));
    };
    println!("  renewed at {renewed} ms");
    std::thread::sleep(Duration::from_secs(2));
    let seen = answers(&std::fs::read(&path)?);
    let last = seen.iter().rfind(|(kind, ..)| *kind == 5).ok_or("no ACK")?;
    println!("  its last ACK gives {}", last.1);
    assert_eq!(last.1, moved, "the guest took the new address");

    step("6. The names it asked for");
    let flows = ok(lab.act("network.flows", Data::new())?)?;
    let names: Vec<String> = flows["lookups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| l["node"] == "guest")
        .map(|l| format!("{} -> {}", l["name"], l["answer"]))
        .collect();
    println!("  {} names asked by the guest: {names:?}", names.len());
    println!("\n  DHCP: pool, lease, DNS and domain reach a real guest; it moves on renewing.");
    Ok(())
}
