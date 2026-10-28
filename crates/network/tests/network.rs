//! A network sandbox through the engine, with two pretend guests: sockets
//! bound where `network.attach` said each card's end is, speaking the same
//! datagrams QEMU's `-netdev dgram` does. Everything a real guest would do
//! first -- DHCP, ARP for the gateway, a name, a ping -- in milliseconds and
//! without QEMU. The same walk with real guests is
//! `crates/api/examples/network_walk.rs`.
#![cfg(unix)]

use std::net::Ipv4Addr;
use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use beatbox_engine::{
    ActionOutcome, Data, Recorder, RuntimeParts, Sandbox, SandboxConfig, SandboxRegistry,
    SandboxRuntime, data,
};
use beatbox_network::frame::{self, GATEWAY_MAC, Mac};
use beatbox_network::{CAPABILITIES, NetworkSandbox, actions};

struct Guest {
    socket: UnixDatagram,
    switch: String,
    mac: Mac,
    address: Ipv4Addr,
}

impl Guest {
    fn cable(network: &Sandbox, node: &str) -> Self {
        let answer = ok(network
            .act("network.attach", data!({"node": node}))
            .unwrap());
        let text = |key: &str| answer[key].as_str().unwrap().to_owned();
        let socket = UnixDatagram::bind(text("port")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        let address: Ipv4Addr = text("address").parse().unwrap();
        Self {
            socket,
            switch: text("switch"),
            mac: frame::node_mac(address),
            address,
        }
    }

    fn send(&self, bytes: &[u8]) {
        self.socket.send_to(bytes, &self.switch).unwrap();
    }

    fn receive(&self) -> Option<Vec<u8>> {
        let mut buffer = [0u8; 2048];
        let size = self.socket.recv(&mut buffer).ok()?;
        Some(buffer[..size].to_vec())
    }

    fn ip(&self, to_mac: Mac, to: Ipv4Addr, protocol: u8, payload: &[u8]) {
        let packet = frame::ipv4_packet(self.address, to, protocol, payload);
        self.send(&frame::ethernet(
            to_mac,
            self.mac,
            frame::ETHER_IPV4,
            &packet,
        ));
    }

    fn ping(&self, other: &Guest) {
        self.ip(
            other.mac,
            other.address,
            frame::ICMP,
            &[8, 0, 0, 0, 0, 7, 0, 1],
        );
    }
}

/// A successful answer, holding what `actions()` promises for it.
fn ok(outcome: ActionOutcome) -> Data {
    assert!(outcome.ok, "{:?}", outcome.reason);
    let spec = actions()
        .into_iter()
        .find(|spec| spec.capability == outcome.capability())
        .unwrap_or_else(|| panic!("{} has no spec", outcome.capability()));
    spec.assert_outputs(&outcome.data);
    outcome.data
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
    SandboxRegistry::default()
        .register(NetworkSandbox::spec())
        .unwrap();
}

fn network(settings: Data) -> (SandboxRuntime, Sandbox, Recorder, std::path::PathBuf) {
    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let log = Recorder::new(1000);
    runtime.events().subscribe("*", log.sink()).unwrap();
    let room = std::env::temp_dir().join(format!("bbnet-{}-{}", std::process::id(), rand_ish()));
    let mut settings = settings;
    settings.insert("workspace".into(), room.to_string_lossy().into());
    let config = SandboxConfig::new("network")
        .unwrap()
        .named("lab")
        .with_settings(settings);
    let sandbox = runtime.create(config).unwrap();
    sandbox.start().unwrap();
    (runtime, sandbox, log, room)
}

/// Unique within this run: the clock alone is not -- two tests on two
/// threads can read the same nanosecond, share a room, and collide on its
/// `switch.sock` ("Address already in use").
fn rand_ish() -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("{nanos}-{next}")
}

fn names(log: &Recorder) -> Vec<String> {
    log.events().iter().map(|e| e.name().to_owned()).collect()
}

#[test]
fn a_guest_gets_an_address_the_gateway_and_a_name() {
    let (_runtime, lab, log, _room) = network(data!({"dns": {"web.corp.local": "10.10.10.80"}}));
    let a = Guest::cable(&lab, "alpha");
    assert_eq!(a.address, Ipv4Addr::new(10, 10, 10, 10));

    // DHCP: DISCOVER from 0.0.0.0 gets an OFFER of the address attach said.
    let mut discover = vec![0u8; 240];
    discover[0..3].copy_from_slice(&[1, 1, 6]);
    discover[28..34].copy_from_slice(&a.mac);
    discover[236..240].copy_from_slice(&[99, 130, 83, 99]);
    discover.extend_from_slice(&[53, 1, 1, 255]);
    let packet = frame::ipv4_packet(
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        frame::UDP,
        &frame::udp(68, 67, &discover),
    );
    a.send(&frame::ethernet(
        frame::BROADCAST,
        a.mac,
        frame::ETHER_IPV4,
        &packet,
    ));
    let offer = a.receive().expect("an offer");
    let offer = frame::ipv4(&offer).unwrap();
    let bootp = frame::udp_data(offer.payload);
    assert_eq!(bootp[16..20], [10, 10, 10, 10]);
    // The domain, so Ubuntu asks this DNS for `.local` instead of mDNS.
    assert!(bootp.windows(7).any(|w| w == b"\x0f\x05local"));

    // ARP for the gateway.
    let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
    arp.extend_from_slice(&a.mac);
    arp.extend_from_slice(&a.address.octets());
    arp.extend_from_slice(&[0; 6]);
    arp.extend_from_slice(&[10, 10, 10, 1]);
    a.send(&frame::ethernet(
        frame::BROADCAST,
        a.mac,
        frame::ETHER_ARP,
        &arp,
    ));
    let answer = frame::arp(&a.receive().expect("an ARP answer")).unwrap();
    assert_eq!(answer.sender_mac, GATEWAY_MAC);

    // Ping the gateway.
    a.ip(
        GATEWAY_MAC,
        Ipv4Addr::new(10, 10, 10, 1),
        frame::ICMP,
        &[8, 0, 0, 0, 0, 1, 0, 1],
    );
    let echo = a.receive().expect("an echo reply");
    assert_eq!(frame::ipv4(&echo).unwrap().payload[0], 0);

    // A record, and a node's own name.
    for (name, expected) in [
        ("web.corp.local", [10, 10, 10, 80]),
        ("alpha", [10, 10, 10, 10]),
    ] {
        let mut query = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            query.push(label.len() as u8);
            query.extend_from_slice(label.as_bytes());
        }
        query.extend_from_slice(&[0, 0, 1, 0, 1]);
        a.ip(
            GATEWAY_MAC,
            Ipv4Addr::new(10, 10, 10, 1),
            frame::UDP,
            &frame::udp(5353, 53, &query),
        );
        let reply = a.receive().expect("a DNS answer");
        let data = frame::udp_data(frame::ipv4(&reply).unwrap().payload).to_vec();
        assert_eq!(data[data.len() - 4..], expected, "{name}");
    }
    assert!(names(&log).contains(&"dns.query".to_owned()));
    assert!(names(&log).contains(&"network.attached".to_owned()));

    // SOA for `local`: one answer, NOERROR -- what nss-mdns checks before
    // stepping aside -- and not shown as a lookup.
    let mut query = vec![0x12, 0x36, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 5];
    query.extend_from_slice(b"local");
    query.extend_from_slice(&[0, 0, 6, 0, 1]);
    a.ip(
        GATEWAY_MAC,
        Ipv4Addr::new(10, 10, 10, 1),
        frame::UDP,
        &frame::udp(5353, 53, &query),
    );
    let reply = a.receive().expect("a SOA answer");
    let data = frame::udp_data(frame::ipv4(&reply).unwrap().payload).to_vec();
    assert_eq!(data[3] & 0x0f, 0, "NOERROR");
    assert_eq!(data[6..8], [0, 1], "one answer");
    assert_eq!(data[query.len()..query.len() + 4], [0xc0, 12, 0, 6]);
    assert!(
        !log.events()
            .iter()
            .any(|e| e.name() == "dns.query" && e.data()["name"] == "local")
    );

    // Asked again: counted, not said again.
    let mut query = vec![0x12, 0x35, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 5];
    query.extend_from_slice(b"alpha");
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    a.ip(
        GATEWAY_MAC,
        Ipv4Addr::new(10, 10, 10, 1),
        frame::UDP,
        &frame::udp(5353, 53, &query),
    );
    assert!(a.receive().is_some());
    let lookups = ok(lab.act("network.flows", Data::new()).unwrap());
    let alpha = lookups["lookups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "alpha")
        .unwrap()
        .clone();
    assert_eq!(alpha["count"], 2);
    assert_eq!(alpha["answer"], "10.10.10.10");
    let said = log
        .events()
        .iter()
        .filter(|e| e.name() == "dns.query" && e.data()["name"] == "alpha")
        .count();
    assert_eq!(said, 1);
}

#[test]
fn the_phase_3_walk_with_pretend_guests() {
    let (_runtime, lab, log, room) = network(Data::new());
    let a = Guest::cable(&lab, "alpha");
    let b = Guest::cable(&lab, "beta");

    // A reaches B, and the network saw it.
    a.ping(&b);
    assert!(b.receive().is_some());
    assert!(names(&log).contains(&"connection.created".to_owned()));

    // DENY: it stops.
    let rule = ok(lab
        .act(
            "firewall.add",
            data!({"action": "deny", "source": "alpha", "destination": "beta", "protocol": "icmp"}),
        )
        .unwrap());
    a.ping(&b);
    assert!(b.receive().is_none());
    assert!(names(&log).contains(&"packet.blocked".to_owned()));

    // ALLOW: it comes back.
    ok(lab
        .act("firewall.remove", data!({"id": rule["id"]}))
        .unwrap());
    ok(lab
        .act(
            "firewall.add",
            data!({"action": "allow", "source": "alpha", "destination": "beta"}),
        )
        .unwrap());
    a.ping(&b);
    assert!(b.receive().is_some());

    // 150 ms, and it is felt.
    ok(lab
        .act("traffic.configure", data!({"latency_ms": 150}))
        .unwrap());
    let sent = Instant::now();
    a.ping(&b);
    assert!(b.receive().is_some());
    assert!(
        sent.elapsed() >= Duration::from_millis(140),
        "{:?}",
        sent.elapsed()
    );

    // Everything lost.
    ok(lab
        .act(
            "traffic.configure",
            data!({"latency_ms": 0, "loss_percent": 100}),
        )
        .unwrap());
    a.ping(&b);
    assert!(b.receive().is_none());
    let facts = lab.refresh().unwrap().facts;
    assert!(facts["counters"]["lost"].as_u64().unwrap() >= 1);
    assert!(facts["counters"]["blocked"].as_u64().unwrap() >= 1);

    // Reset: rules and traffic as configured, cables where they were.
    lab.reset().unwrap();
    lab.start().unwrap();
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["firewall"]["rules"], serde_json::json!([]));
    assert_eq!(facts["traffic"]["loss_percent"], 0.0);
    assert_eq!(facts["nodes"].as_array().unwrap().len(), 2);
    a.ping(&b);
    assert!(b.receive().is_some());

    let flows = ok(lab.act("network.flows", Data::new()).unwrap());
    assert!(!flows["flows"].as_array().unwrap().is_empty());

    lab.stop().unwrap();
    lab.destroy().unwrap();
    assert!(!room.exists());
}

/// A paused VM stops reading its cable. What piles up for it must not stop
/// the others, nor the network from stopping.
#[test]
fn a_machine_that_stops_reading_holds_up_nobody_else() {
    let (_runtime, lab, _log, room) = network(Data::new());
    let a = Guest::cable(&lab, "alpha");
    let b = Guest::cable(&lab, "beta");
    let paused = Guest::cable(&lab, "paused");
    let mut big = vec![0u8; 1400];
    big[0] = 8;
    // More than a send buffer's worth (208 KiB), and few enough that alpha's
    // own send never waits on a stuck switch.
    for _ in 0..150 {
        a.ip(paused.mac, paused.address, frame::ICMP, &big);
    }
    a.ping(&b);
    assert!(b.receive().is_some());
    lab.stop().unwrap();
    lab.destroy().unwrap();
    assert!(!room.exists());
}

#[test]
fn a_bad_setting_is_refused_before_anything_runs() {
    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    for settings in [
        data!({"cidr": "10.10.10.5/24"}),
        data!({"firewall": {"rules": [{"action": "maybe"}]}}),
        data!({"traffic": {"loss_percent": 150}}),
    ] {
        let config = SandboxConfig::new("network")
            .unwrap()
            .with_settings(settings);
        assert!(runtime.create(config).is_err());
    }
}

#[test]
fn a_network_outlives_the_daemon_that_ran_it() {
    let (runtime, lab, _log, room) = network(Data::new());
    let a = Guest::cable(&lab, "alpha");
    let b = Guest::cable(&lab, "beta");
    ok(lab
        .act(
            "firewall.add",
            data!({"action": "deny", "source": "alpha", "destination": "beta"}),
        )
        .unwrap());
    let (identity, config) = (lab.identity(), lab.config());
    // Killed, not stopped: nothing is dropped, so the switch's socket stays
    // in the room the way it does after a SIGKILL.
    std::mem::forget((runtime, lab));

    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let again = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let lab = again
        .restore(identity, config, beatbox_engine::Lifecycle::Running, None)
        .unwrap();
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(facts["firewall"]["rules"][0]["id"], "r1");
    a.ping(&b);
    assert!(b.receive().is_none(), "the rule came back with it");
    ok(lab.act("firewall.remove", data!({"id": "r1"})).unwrap());
    a.ping(&b);
    assert!(b.receive().is_some(), "and the cables did too");
    lab.stop().unwrap();
    lab.destroy().unwrap();
    assert!(!room.exists());
}

/// A switch already picked back up is started again: the old one must end,
/// and not take the new one's socket with it.
#[test]
fn starting_a_network_whose_switch_runs_keeps_one_switch() {
    let (runtime, lab, _log, room) = network(Data::new());
    let a = Guest::cable(&lab, "alpha");
    let b = Guest::cable(&lab, "beta");
    let (identity, config) = (lab.identity(), lab.config());
    std::mem::forget((runtime, lab));

    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let again = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let lab = again
        .restore(identity, config, beatbox_engine::Lifecycle::Stopped, None)
        .unwrap();
    // Seeing the socket left behind, it picks the switch back up.
    lab.refresh().unwrap();
    lab.start().unwrap();
    a.ping(&b);
    assert!(b.receive().is_some());
    lab.stop().unwrap();
    lab.destroy().unwrap();
    assert!(!room.exists());
}

#[test]
fn the_default_can_be_changed_after_the_network_is_made() {
    let (_runtime, lab, _log, _room) = network(Data::new());
    let a = Guest::cable(&lab, "alpha");
    let b = Guest::cable(&lab, "beta");

    // Nothing fits: allowed, as made.
    a.ping(&b);
    assert!(b.receive().is_some());

    ok(lab
        .act("firewall.default", data!({"policy": "deny"}))
        .unwrap());
    a.ping(&b);
    assert!(b.receive().is_none(), "nothing fits, so nothing talks");
    let facts = ok(lab.act("network.flows", Data::new()).unwrap());
    assert!(facts.contains_key("flows"));

    ok(lab
        .act("firewall.default", data!({"policy": "allow"}))
        .unwrap());
    a.ping(&b);
    assert!(b.receive().is_some());

    // Anything else is refused and changes nothing.
    assert!(
        !lab.act("firewall.default", data!({"policy": "maybe"}))
            .unwrap()
            .ok
    );
    a.ping(&b);
    assert!(b.receive().is_some());
}

/// This computer's own address on its network, which a guest may reach
/// through the router as it would anything outside; `None` with no route.
fn this_computer() -> Option<Ipv4Addr> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // Connecting a UDP socket sends nothing; it only picks the way out.
    probe.connect("192.0.2.1:9").ok()?;
    match probe.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(address) if !address.is_loopback() => Some(address),
        _ => None,
    }
}

#[test]
fn a_guest_reaches_outside_through_the_router_and_hears_the_answer() {
    let Some(here) = this_computer() else {
        eprintln!("no way out on this computer: skipped");
        return;
    };
    let (_runtime, lab, _log, _room) = network(Data::new());
    let a = Guest::cable(&lab, "alpha");
    let far = std::net::UdpSocket::bind((here, 0)).unwrap();
    let port = far.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 64];
        while let Ok((size, from)) = far.recv_from(&mut buffer) {
            let _ = far.send_to(&buffer[..size], from);
        }
    });
    let out = |a: &Guest| {
        a.ip(
            GATEWAY_MAC,
            here,
            frame::UDP,
            &frame::udp(40000, port, b"hello"),
        )
    };

    // Off: nothing comes back.
    out(&a);
    assert!(a.receive().is_none());

    ok(lab.act("router.internet", data!({"on": true})).unwrap());
    out(&a);
    let mut heard = None;
    for _ in 0..10 {
        if let Some(bytes) = a.receive() {
            heard = Some(bytes);
            break;
        }
    }
    let bytes = heard.expect("the answer, from outside");
    let back = frame::ipv4(&bytes).unwrap();
    assert_eq!(back.flow.source, here);
    assert_eq!(back.flow.source_port, port);
    assert_eq!(back.flow.destination, a.address);
    assert_eq!(frame::udp_data(back.payload), b"hello");

    // A rule against the internet stops it again.
    ok(lab
        .act(
            "firewall.add",
            data!({"action": "deny", "source": "alpha", "destination": "internet"}),
        )
        .unwrap());
    out(&a);
    assert!(a.receive().is_none());
}

/// What the page does to move a network: ask for the plan, then reconfigure
/// with the settings the plan says. Returns the plan.
fn renumber(lab: &Sandbox, cidr: &str) -> Data {
    ok(lab
        .act("network.renumber_plan", data!({"cidr": cidr}))
        .unwrap())
}

#[test]
fn a_network_is_moved_to_another_range_with_its_names_rules_and_cables() {
    let (runtime, lab, _log, _room) = network(data!({
        "cidr": "10.50.20.0/24",
        "firewall": {"default": "deny",
                     "rules": [{"action": "allow", "source": "10.50.20.0/24", "destination": "alpha"}]},
        "dns": {"portal.local": "10.50.20.11"},
    }));
    let a = Guest::cable(&lab, "alpha");
    let b = Guest::cable(&lab, "beta");
    assert_eq!(
        (a.address, b.address),
        (Ipv4Addr::new(10, 50, 20, 10), Ipv4Addr::new(10, 50, 20, 11))
    );
    ok(lab
        .act(
            "firewall.add",
            data!({"action": "allow", "source": "beta", "destination": "10.50.20.10"}),
        )
        .unwrap());

    // The plan says what moves, and changes nothing.
    let plan = renumber(&lab, "10.60.0.0/24");
    assert_eq!(plan["gateway"], "10.60.0.1");
    assert_eq!(plan["nodes"][1]["to"], "10.60.0.11");
    assert_eq!(plan["names"][0]["to"], "10.60.0.11");
    assert_eq!(
        plan["rules"].as_array().unwrap().len(),
        2,
        "one end of each of the two rules"
    );
    assert_eq!(lab.refresh().unwrap().facts["cidr"], "10.50.20.0/24");

    // Nothing runs while it changes: stopped first, as the engine requires.
    let refused = lab.reconfigure(lab.config().with_settings(data!({"cidr": "10.60.0.0/24"})));
    assert!(refused.is_err(), "a running network is not reconfigured");
    lab.stop().unwrap();
    // The range alone, with the names and rules left behind: not what the plan said.
    let careless = lab.reconfigure(lab.config().with_settings({
        let mut settings = lab.config().settings.clone();
        settings.insert("cidr".into(), "10.60.0.0/24".into());
        settings
    }));
    assert!(
        careless.is_err(),
        "the names and rules have to move with it"
    );
    assert_eq!(
        lab.refresh().unwrap().facts["cidr"],
        "10.50.20.0/24",
        "nothing changed"
    );

    // What the plan says, applied.
    let moved = {
        let settings = lab.config().settings.clone();
        let mut settings = settings;
        settings.insert("cidr".into(), "10.60.0.0/24".into());
        settings.insert("dns".into(), data!({"portal.local": "10.60.0.11"}).into());
        settings.insert(
            "firewall".into(),
            data!({"default": "deny",
                   "rules": [{"action": "allow", "source": "10.60.0.0/24", "destination": "alpha"}]})
            .into(),
        );
        settings
    };
    lab.reconfigure(lab.config().with_settings(moved)).unwrap();
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["cidr"], "10.60.0.0/24");
    assert_eq!(facts["gateway"], "10.60.0.1");
    let nodes = facts["nodes"].as_array().unwrap();
    assert_eq!(nodes[0]["address"], "10.60.0.10");
    assert_eq!(nodes[1]["address"], "10.60.0.11");
    assert_eq!(
        nodes[1]["mac"],
        frame::mac_text(b.mac),
        "the machine's cable is still valid"
    );
    assert_eq!(facts["dns"]["portal.local"], "10.60.0.11");
    let rules = facts["firewall"]["rules"].as_array().unwrap();
    assert_eq!(rules[0]["source"], "10.60.0.0/24");
    assert_eq!(rules.last().unwrap()["destination"], "10.60.0.10");
    drop(runtime);
}

#[test]
fn a_moved_network_is_found_moved_by_the_daemon_that_follows() {
    let (runtime, lab, _log, _room) = network(data!({"cidr": "10.50.20.0/24"}));
    let a = Guest::cable(&lab, "alpha");
    lab.stop().unwrap();
    let mut settings = lab.config().settings.clone();
    settings.insert("cidr".into(), "10.60.0.0/24".into());
    lab.reconfigure(lab.config().with_settings(settings))
        .unwrap();
    let (identity, config) = (lab.identity(), lab.config());
    std::mem::forget((runtime, lab));

    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let again = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let lab = again
        .restore(identity, config, beatbox_engine::Lifecycle::Stopped, None)
        .unwrap();
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["cidr"], "10.60.0.0/24");
    let node = &facts["nodes"].as_array().unwrap()[0];
    assert_eq!(node["address"], "10.60.0.10");
    assert_eq!(
        node["mac"],
        frame::mac_text(a.mac),
        "its own MAC, not one worked out from the new address"
    );
}

#[test]
fn the_plan_refuses_what_cannot_be_done_and_changes_nothing() {
    let (_runtime, lab, _log, _room) = network(Data::new());
    Guest::cable(&lab, "alpha");
    for (cidr, code) in [
        ("10.10.10.0/24", "network.renumber_same"),
        ("10.10.10.5/24", "network.bad_cidr"),
        ("nonsense", "network.bad_cidr"),
    ] {
        let outcome = lab
            .act("network.renumber_plan", data!({"cidr": cidr}))
            .unwrap();
        assert!(!outcome.ok, "{cidr}");
        assert_eq!(outcome.reason.unwrap().code, code, "{cidr}");
    }
}

#[test]
fn a_switch_with_two_vlans_is_built_through_the_api_and_kept() {
    let (runtime, lab, _log, _room) = network(data!({"cidr": "10.50.20.0/24"}));
    ok(lab
        .act("device.add", data!({"kind": "switch", "name": "sw1"}))
        .unwrap());
    let link = ok(lab.act("link.add", data!({"a": "gw", "b": "sw1"})).unwrap());
    assert_eq!(link["b"], "sw1:0");
    // Two machines on sw1 in VLAN 10, one in VLAN 20.
    for (port, vlan) in [("sw1:1", 10), ("sw1:2", 10), ("sw1:3", 20)] {
        ok(lab
            .act(
                "port.configure",
                data!({"port": port, "mode": "access", "vlan": vlan}),
            )
            .unwrap());
    }
    let place = |node: &str, at: &str| {
        let answer = ok(lab
            .act("network.attach", data!({"node": node, "at": at}))
            .unwrap());
        let socket = UnixDatagram::bind(answer["port"].as_str().unwrap()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        let address: Ipv4Addr = answer["address"].as_str().unwrap().parse().unwrap();
        Guest {
            socket,
            switch: answer["switch"].as_str().unwrap().to_owned(),
            mac: frame::node_mac(address),
            address,
        }
    };
    let b = place("b", "sw1:1");
    let c = place("c", "sw1:2");
    let d = place("d", "sw1:3");
    b.ping(&c);
    assert!(c.receive().is_some(), "VLAN 10 to VLAN 10");
    b.ping(&d);
    assert!(d.receive().is_none(), "VLAN 10 to VLAN 20");

    // What the page reads.
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["topology"]["cables"][0][1], "sw1:0");
    assert_eq!(facts["topology"]["ports"]["sw1:3"]["vlan"], 20);
    assert_eq!(facts["nodes"][2]["at"], "sw1:3");
    // A machine plugged in without a port goes to the switch, not the router.
    ok(lab.act("network.attach", data!({"node": "e"})).unwrap());
    assert_eq!(lab.refresh().unwrap().facts["nodes"][3]["at"], "sw1:4");

    // Refusals say why and change nothing.
    let code = |capability: &str, arguments: Data| {
        lab.act(capability, arguments).unwrap().reason.unwrap().code
    };
    assert_eq!(
        code("device.remove", data!({"device": "gw"})),
        "network.last_router"
    );
    // A switch goes with its cables; a machine on it moves to the router.
    lab.act("device.add", data!({"kind": "switch", "name": "sw9"}))
        .unwrap();
    ok(lab
        .act("network.attach", data!({"node": "f", "at": "sw9"}))
        .unwrap());
    lab.act("link.add", data!({"a": "gw", "b": "sw9"})).unwrap();
    let cables = lab.refresh().unwrap().facts["topology"]["cables"]
        .as_array()
        .unwrap()
        .len();
    lab.act("device.remove", data!({"device": "sw9"})).unwrap();
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(
        facts["topology"]["cables"].as_array().unwrap().len(),
        cables - 1
    );
    assert_eq!(facts["topology"]["devices"].as_array().unwrap().len(), 2);
    let f = facts["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "f")
        .unwrap();
    assert!(
        f["at"].as_str().unwrap().starts_with("gw:"),
        "f moved to the router"
    );
    assert_eq!(
        code("link.add", data!({"a": "gw:0", "b": "sw1:5"})),
        "network.port_taken"
    );
    assert_eq!(
        code("link.add", data!({"a": "gw", "b": "sw1"})),
        "network.loop"
    );
    assert_eq!(
        code(
            "port.configure",
            data!({"port": "sw1:1", "mode": "access", "vlan": 5000})
        ),
        "network.bad_vlan"
    );
    assert_eq!(
        code("device.add", data!({"kind": "hub", "name": "h1"})),
        "network.device_kind"
    );
    assert_eq!(
        code("device.add", data!({"kind": "switch", "name": "b"})),
        "network.device_taken"
    );

    // Moved to VLAN 20's port, b reaches d.
    ok(lab
        .act(
            "port.configure",
            data!({"port": "sw1:5", "mode": "access", "vlan": 20}),
        )
        .unwrap());
    ok(lab
        .act("node.place", data!({"node": "b", "at": "sw1:5"}))
        .unwrap());
    b.ping(&d);
    assert!(d.receive().is_some(), "now both in VLAN 20");

    // A daemon that follows finds the same topology.
    let (identity, config) = (lab.identity(), lab.config());
    std::mem::forget((runtime, lab));
    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let again = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let lab = again
        .restore(identity, config, beatbox_engine::Lifecycle::Running, None)
        .unwrap();
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["topology"]["devices"][1]["name"], "sw1");
    assert_eq!(facts["nodes"][0]["at"], "sw1:5");
    assert_eq!(facts["topology"]["ports"]["sw1:5"]["vlan"], 20);
}

#[test]
fn two_vlans_with_networks_on_the_router_reach_each_other_through_it() {
    let (runtime, lab, _log, _room) = network(data!({"cidr": "10.50.20.0/24"}));
    ok(lab
        .act("device.add", data!({"kind": "switch", "name": "sw1"}))
        .unwrap());
    ok(lab.act("link.add", data!({"a": "gw", "b": "sw1"})).unwrap());
    let trunk = |port: &str| {
        ok(lab
            .act(
                "port.configure",
                data!({"port": port, "mode": "trunk", "native": 1, "allowed": [10, 20]}),
            )
            .unwrap());
    };
    trunk("gw:0");
    trunk("sw1:0");
    for (port, vlan) in [("sw1:1", 10), ("sw1:2", 20)] {
        ok(lab
            .act(
                "port.configure",
                data!({"port": port, "mode": "access", "vlan": vlan}),
            )
            .unwrap());
    }
    for (vlan, cidr) in [(10, "10.50.10.0/24"), (20, "10.50.30.0/24")] {
        let done = ok(lab
            .act("interface.configure", data!({"vlan": vlan, "cidr": cidr}))
            .unwrap());
        assert_eq!(done["moved"].as_array().unwrap().len(), 0);
    }
    let place = |node: &str, at: &str| {
        let answer = ok(lab
            .act("network.attach", data!({"node": node, "at": at}))
            .unwrap());
        let socket = UnixDatagram::bind(answer["port"].as_str().unwrap()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        let address: Ipv4Addr = answer["address"].as_str().unwrap().parse().unwrap();
        (
            Guest {
                socket,
                switch: answer["switch"].as_str().unwrap().to_owned(),
                mac: frame::parse_mac(answer["mac"].as_str().unwrap()).unwrap(),
                address,
            },
            answer,
        )
    };
    // Each is given an address in its own VLAN's network, with its gateway.
    let (b, said) = place("b", "sw1:1");
    assert_eq!(said["address"], "10.50.10.10");
    assert_eq!(said["gateway"], "10.50.10.1");
    let (d, said) = place("d", "sw1:2");
    assert_eq!(said["address"], "10.50.30.10");
    assert_eq!(said["gateway"], "10.50.30.1");
    // b pings d through the router: it arrives from the router, one hop older.
    b.ip(
        GATEWAY_MAC,
        d.address,
        frame::ICMP,
        &[8, 0, 0, 0, 0, 1, 0, 1],
    );
    let got = d.receive().expect("routed to d");
    assert_eq!(frame::source(&got), GATEWAY_MAC);
    assert_eq!(got[14 + 8], 63);

    // A slow cable to d, and a lossy one between the router and sw1.
    ok(lab
        .act("cable.configure", data!({"cable": "d", "latency_ms": 40}))
        .unwrap());
    ok(lab
        .act(
            "cable.configure",
            data!({"cable": "sw1:0", "loss_percent": 5}),
        )
        .unwrap());
    let cables = &lab.refresh().unwrap().facts["cable_traffic"];
    assert_eq!(cables[0]["node"], "d");
    assert_eq!(cables[0]["latency_ms"], 40.0);
    assert_eq!(cables[1]["a"], "gw:0");
    assert_eq!(cables[1]["loss_percent"], 5.0);

    // A rule and a name that point into VLAN 20's network.
    ok(lab
        .act(
            "firewall.add",
            data!({"action": "allow", "source": "b", "destination": "10.50.30.0/24"}),
        )
        .unwrap());
    ok(lab
        .act(
            "dns.add",
            data!({"name": "files.local", "address": "10.50.30.10"}),
        )
        .unwrap());

    // VLAN 20's network moves: d moves with it, and says so.
    let done = ok(lab
        .act(
            "interface.configure",
            data!({"vlan": 20, "cidr": "10.50.40.0/24"}),
        )
        .unwrap());
    assert_eq!(done["moved"][0]["node"], "d");
    assert_eq!(done["moved"][0]["to"], "10.50.40.10");
    // d's address, chosen: inside its network and free, or refused.
    let code = |capability: &str, arguments: Data| {
        lab.act(capability, arguments).unwrap().reason.unwrap().code
    };
    assert_eq!(
        code(
            "node.address",
            data!({"node": "d", "address": "10.50.10.50"})
        ),
        "network.address_outside"
    );
    ok(lab
        .act(
            "node.address",
            data!({"node": "d", "address": "10.50.40.50"}),
        )
        .unwrap());
    ok(lab
        .act(
            "node.address",
            data!({"node": "d", "address": "10.50.40.10"}),
        )
        .unwrap());
    // The rule and the name went along.
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(
        facts["firewall"]["rules"][0]["destination"],
        "10.50.40.0/24"
    );
    assert_eq!(facts["dns"]["files.local"], "10.50.40.10");
    // Refusals.
    let code = |capability: &str, arguments: Data| {
        lab.act(capability, arguments).unwrap().reason.unwrap().code
    };
    assert_eq!(
        code(
            "interface.configure",
            data!({"vlan": 30, "cidr": "10.50.20.128/25"})
        ),
        "network.overlap"
    );
    assert_eq!(
        code("network.renumber_plan", data!({"cidr": "10.50.10.0/24"})),
        "network.overlap"
    );
    assert_eq!(
        code("interface.remove", data!({"vlan": 30})),
        "network.no_such_interface"
    );
    // Renumbering the main range leaves the other networks' machines be.
    let plan = ok(lab
        .act("network.renumber_plan", data!({"cidr": "10.60.0.0/24"}))
        .unwrap());
    assert_eq!(plan["nodes"].as_array().unwrap().len(), 0);

    ok(lab
        .act("vlan.name", data!({"vlan": 10, "name": "Gestão"}))
        .unwrap());
    assert_eq!(lab.refresh().unwrap().facts["vlan_names"]["10"], "Gestão");

    // A daemon that follows finds the networks.
    let (identity, config) = (lab.identity(), lab.config());
    std::mem::forget((runtime, lab));
    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let again = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let lab = again
        .restore(identity, config, beatbox_engine::Lifecycle::Running, None)
        .unwrap();
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["interfaces"][1]["cidr"], "10.50.40.0/24");
    assert_eq!(facts["vlan_names"]["10"], "Gestão");
    assert_eq!(facts["cable_traffic"][1]["b"], "sw1:0");
    assert_eq!(facts["nodes"][1]["address"], "10.50.40.10");
}

#[test]
fn a_second_router_with_its_own_network_is_reached_by_routes() {
    let (runtime, lab, _log, _room) = network(data!({"cidr": "10.50.20.0/24"}));
    let act = |capability: &str, arguments: Data| ok(lab.act(capability, arguments).unwrap());
    act("device.add", data!({"kind": "router", "name": "r2"}));
    act("link.add", data!({"a": "gw", "b": "r2"}));
    for port in ["gw:0", "r2:0"] {
        act(
            "port.configure",
            data!({"port": port, "mode": "access", "vlan": 99}),
        );
    }
    act(
        "interface.configure",
        data!({"vlan": 99, "cidr": "10.99.0.0/24"}),
    );
    act(
        "interface.configure",
        data!({"router": "r2", "vlan": 99, "cidr": "10.99.0.0/24", "address": "10.99.0.2"}),
    );
    act(
        "interface.configure",
        data!({"router": "r2", "vlan": 1, "cidr": "10.20.0.0/24"}),
    );
    act(
        "route.add",
        data!({"to": "10.20.0.0/24", "via": "10.99.0.2"}),
    );
    act(
        "route.add",
        data!({"router": "r2", "to": "0.0.0.0/0", "via": "10.99.0.1"}),
    );
    let place = |node: &str, at: &str| {
        let answer = act("network.attach", data!({"node": node, "at": at}));
        let socket = UnixDatagram::bind(answer["port"].as_str().unwrap()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        (
            Guest {
                socket,
                switch: answer["switch"].as_str().unwrap().to_owned(),
                mac: frame::parse_mac(answer["mac"].as_str().unwrap()).unwrap(),
                address: answer["address"].as_str().unwrap().parse().unwrap(),
            },
            answer,
        )
    };
    let (a, _) = place("a", "gw");
    let (c, said) = place("c", "r2");
    // c is r2's: its address and gateway come from r2's network.
    assert_eq!(said["address"], "10.20.0.10");
    assert_eq!(said["gateway"], "10.20.0.1");
    // a reaches c through both routers.
    a.ip(
        GATEWAY_MAC,
        c.address,
        frame::ICMP,
        &[8, 0, 0, 0, 0, 1, 0, 1],
    );
    let got = c.receive().expect("routed twice to c");
    assert_eq!(got[14 + 8], 62);
    // A route whose next hop is on none of the router's networks.
    assert_eq!(
        lab.act(
            "route.add",
            data!({"router": "r2", "to": "172.16.0.0/12", "via": "10.50.20.9"})
        )
        .unwrap()
        .reason
        .unwrap()
        .code,
        "network.route_via"
    );

    // A daemon that follows finds both routers as they were.
    let (identity, config) = (lab.identity(), lab.config());
    std::mem::forget((runtime, lab));
    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    let again = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let lab = again
        .restore(identity, config, beatbox_engine::Lifecycle::Running, None)
        .unwrap();
    let facts = lab.refresh().unwrap().facts;
    let r2 = &facts["routers"][1];
    assert_eq!(r2["name"], "r2");
    assert_eq!(r2["networks"][0]["cidr"], "10.20.0.0/24");
    assert_eq!(r2["networks"][1]["address"], "10.99.0.2");
    assert_eq!(r2["routes"][0]["to"], "0.0.0.0/0");
    assert_eq!(facts["routers"][0]["routes"][0]["via"], "10.99.0.2");
}

/// A light machine's terminal line, retried until it says something other
/// than "no address yet" (DHCP takes a moment).
fn run(lab: &Sandbox, node: &str, line: &str) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let said = ok(lab
            .act("light.run", data!({"node": node, "line": line}))
            .unwrap());
        let lines = said["lines"].as_array().unwrap().clone();
        if lines.first().is_none_or(|l| l["code"] != "no_address") || Instant::now() > deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn light_machines_get_an_address_ping_look_names_up_and_fetch_a_page() {
    let (_runtime, lab, _log, _room) = network(data!({"cidr": "10.50.20.0/24"}));
    for (node, kind) in [("pc1", "pc"), ("pc2", "pc"), ("srv1", "server")] {
        let made = ok(lab
            .act("light.add", data!({"node": node, "kind": kind, "at": "gw"}))
            .unwrap());
        assert_eq!(made["light"]["kind"], kind);
    }
    // Its address by DHCP, from the router.
    let said = run(&lab, "pc1", "ipconfig");
    assert_eq!(said[0]["code"], "ipconfig", "{said:?}");
    assert_eq!(said[0]["address"], "10.50.20.10");
    assert_eq!(said[0]["gateway"], "10.50.20.1");
    run(&lab, "pc2", "ipconfig");
    run(&lab, "srv1", "ipconfig");
    // A name, asked of the router.
    let said = run(&lab, "pc1", "nslookup pc2");
    assert_eq!(said[0]["code"], "dns.answer", "{said:?}");
    assert_eq!(said[0]["address"], "10.50.20.11");
    // Ping by name: three answers.
    let said = run(&lab, "pc1", "ping pc2");
    let summary = said.iter().find(|l| l["code"] == "ping.summary").unwrap();
    assert_eq!(summary["received"], 3, "{said:?}");
    let fast = said
        .iter()
        .filter(|l| l["code"] == "ping.reply")
        .skip(1)
        .all(|l| l["ms"].as_u64().unwrap() < 15);
    assert!(fast, "a ping inside one network takes no time: {said:?}");
    // The server's page.
    let said = run(&lab, "pc1", "curl srv1");
    let page = said
        .iter()
        .find(|l| l["code"] == "curl.answer")
        .expect("a page");
    assert_eq!(page["title"], "srv1", "{said:?}");
    // A port: open where the server listens, refused where it does not.
    let said = run(&lab, "pc1", "nc srv1 80");
    assert_eq!(said.last().unwrap()["code"], "tcp.open", "{said:?}");
    assert_eq!(said.last().unwrap()["port"], 80);
    let said = run(&lab, "pc1", "nc 10.50.20.12 81");
    assert_eq!(said.last().unwrap()["code"], "curl.refused", "{said:?}");
    assert_eq!(run(&lab, "pc1", "nc srv1")[0]["code"], "needs");
    // traceroute to the gateway: one hop, and it is the end.
    let said = run(&lab, "pc1", "traceroute 10.50.20.1");
    assert_eq!(said[0]["code"], "trace.hop", "{said:?}");
    assert_eq!(said[0]["from"], "10.50.20.1");
    // Under Deny, nobody answers, and the terminal says it was the firewall.
    ok(lab
        .act("firewall.default", data!({"policy": "deny"}))
        .unwrap());
    let said = run(&lab, "pc1", "ping 10.50.20.11");
    assert_eq!(
        said.iter().filter(|l| l["code"] == "ping.timeout").count(),
        3,
        "{said:?}"
    );
    assert_eq!(said.last().unwrap()["code"], "why.firewall", "{said:?}");
    // What the page reads, and an unknown command.
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["nodes"][2]["light"]["web"], true);
    assert_eq!(run(&lab, "pc1", "frobnicate")[0]["code"], "unknown");
    // Renamed: the machine answers to its new name, rules and cable follow.
    ok(lab
        .act(
            "firewall.add",
            data!({"action": "allow", "source": "pc1", "destination": "pc2"}),
        )
        .unwrap());
    ok(lab
        .act("node.rename", data!({"node": "pc1", "name": "recepcao"}))
        .unwrap());
    let facts = lab.refresh().unwrap().facts;
    assert!(
        facts["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["node"] == "recepcao")
    );
    assert_eq!(
        facts["firewall"]["rules"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["source"],
        "recepcao"
    );
    let said = run(&lab, "recepcao", "ping 10.50.20.1");
    assert_eq!(
        said.iter().find(|l| l["code"] == "ping.summary").unwrap()["received"],
        3,
        "{said:?}"
    );
    let code = |capability: &str, arguments: Data| {
        lab.act(capability, arguments).unwrap().reason.unwrap().code
    };
    assert_eq!(
        code("node.rename", data!({"node": "recepcao", "name": "pc2"})),
        "network.device_taken"
    );
    assert_eq!(
        code(
            "node.rename",
            data!({"node": "recepcao", "name": "Bad Name"})
        ),
        "network.bad_device"
    );
    ok(lab
        .act(
            "device.rename",
            data!({"device": "gw", "name": "router-sede"}),
        )
        .unwrap());
    assert_eq!(
        lab.refresh().unwrap().facts["topology"]["devices"][0]["name"],
        "router-sede"
    );
    assert_eq!(
        code("device.rename", data!({"device": "nope", "name": "x1"})),
        "network.no_such_device"
    );

    // In the background: started, asked after until it is done.
    let started = ok(lab
        .act(
            "light.start",
            data!({"node": "recepcao", "line": "ping 10.50.20.1"}),
        )
        .unwrap());
    let job = started["job"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let lines = loop {
        let said = ok(lab.act("light.result", data!({"job": job})).unwrap());
        if said["done"] == true {
            break said["lines"].as_array().unwrap().clone();
        }
        assert!(Instant::now() < deadline, "the job never finished");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        lines.iter().any(|l| l["code"] == "ping.summary"),
        "{lines:?}"
    );
    assert_eq!(
        lab.act("light.result", data!({"job": job}))
            .unwrap()
            .reason
            .unwrap()
            .code,
        "network.no_such_job",
        "an answer is taken once"
    );
}

/// A network written to a file and made again from it elsewhere; a save
/// point taken, the network changed, and brought back to it -- with the
/// virtual machine cabled to it left where it was (issue #58).
#[test]
fn a_network_is_exported_imported_and_brought_back_to_a_save_point() {
    let (_runtime, lab, _log, _room) = network(data!({"cidr": "10.60.0.0/24"}));
    ok(lab
        .act("device.add", data!({"kind": "switch", "name": "sw1"}))
        .unwrap());
    ok(lab.act("link.add", data!({"a": "gw", "b": "sw1"})).unwrap());
    ok(lab
        .act(
            "light.add",
            data!({"node": "srv1", "kind": "server", "at": "sw1"}),
        )
        .unwrap());
    let _alpha = Guest::cable(&lab, "alpha");
    ok(lab
        .act(
            "firewall.add",
            data!({"action": "deny", "source": "alpha", "destination": "srv1"}),
        )
        .unwrap());
    ok(lab
        .act(
            "dns.add",
            data!({"name": "files.lab", "address": "10.60.0.9"}),
        )
        .unwrap());
    // srv1's lease lands in the export when its DHCP answer comes; let it
    // come first, or the exports below can differ from this one.
    assert_eq!(settles(&lab, "srv1", "10.60.0.10")["address"], "10.60.0.10");

    let state = ok(lab.act("network.export", Data::new()).unwrap())["state"].clone();
    // Saved where the person chose, under their name, never over a file.
    let folder = std::env::temp_dir().join(format!("bbexport-{}", rand_ish()));
    std::fs::create_dir_all(&folder).unwrap();
    let to = |name: &str| {
        ok(lab
            .act(
                "network.export",
                data!({"file": true, "folder": folder.to_string_lossy(), "name": name}),
            )
            .unwrap())["path"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(to("aula 3.json").ends_with("aula-3.bbn"));
    assert!(to("aula 3").ends_with("aula-3-2.bbn"));
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(to("x")).unwrap()).unwrap();
    assert_eq!(saved, state);
    assert!(
        !lab.act(
            "network.export",
            data!({"file": true, "folder": folder.join("nope").to_string_lossy()}),
        )
        .unwrap()
        .ok
    );
    let _ = std::fs::remove_dir_all(&folder);
    assert_eq!(state["format"], "beatbox-network");
    assert!(state.get("captures").is_none() && state.get("counters").is_none());

    // Elsewhere: the devices, the light machine, the rule and the name; not
    // the virtual machine, which the file only remembers.
    let (_r2, copy, _l2, _room2) = network(data!({"cidr": "10.60.0.0/24"}));
    let made = ok(copy
        .act("network.import", data!({"state": state.clone()}))
        .unwrap());
    assert_eq!(made["devices"], 2);
    assert_eq!(made["lights"], 1);
    let facts = copy.refresh().unwrap().facts;
    let nodes: Vec<&str> = facts["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["node"].as_str().unwrap())
        .collect();
    assert_eq!(nodes, ["srv1"]);
    assert_eq!(facts["firewall"]["rules"][0]["destination"], "srv1");
    assert_eq!(facts["dns"]["files.lab"], "10.60.0.9");

    // Not a network file, or one from a later BeatBox: refused, nothing changes.
    let code = |sandbox: &Sandbox, capability: &str, arguments: Data| {
        sandbox
            .act(capability, arguments)
            .unwrap()
            .reason
            .unwrap()
            .code
    };
    assert_eq!(
        code(&copy, "network.import", data!({"state": {"nodes": []}})),
        "network.bad_file"
    );
    let mut later = state.clone();
    later["version"] = 99.into();
    assert_eq!(
        code(&copy, "network.import", data!({"state": later})),
        "network.file_too_new"
    );
    let mut broken = state.clone();
    broken["firewall"]["rules"][0]["action"] = "maybe".into();
    assert!(
        !copy
            .act("network.import", data!({"state": broken}))
            .unwrap()
            .ok
    );
    assert_eq!(
        copy.refresh().unwrap().facts["firewall"]["rules"],
        facts["firewall"]["rules"]
    );

    // A save point, a change, and back.
    let point = ok(lab.act("point.save", data!({"name": "antes"})).unwrap())["id"].clone();
    ok(lab
        .act("device.add", data!({"kind": "switch", "name": "sw2"}))
        .unwrap());
    ok(lab.act("firewall.remove", data!({"id": "r1"})).unwrap());
    let list = ok(lab.act("point.list", Data::new()).unwrap());
    assert_eq!(list["points"][0]["name"], "antes");
    ok(lab
        .act("point.restore", data!({"id": point.clone()}))
        .unwrap());
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["topology"]["devices"].as_array().unwrap().len(), 2);
    assert_eq!(facts["firewall"]["rules"][0]["id"], "r1");
    let alpha = facts["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "alpha")
        .expect("the virtual machine stays");
    assert_eq!(alpha["at"], "sw1:2");

    assert_eq!(
        code(&lab, "point.restore", data!({"id": "../net"})),
        "network.no_such_point"
    );
    ok(lab.act("point.remove", data!({"id": point})).unwrap());
    let list = ok(lab.act("point.list", Data::new()).unwrap());
    assert!(list["points"].as_array().unwrap().is_empty());
}

/// The address light machine `node` holds once it settles on `want`.
fn settles(lab: &Sandbox, node: &str, want: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let said = run(lab, node, "ipconfig");
        if said[0]["address"] == want || Instant::now() > deadline {
            return said[0].clone();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The router as a names server with records and a domain, and as a DHCP
/// server that hands out what it is told -- with the Server's names and
/// virtual machines left alone (issue #52).
#[test]
fn the_router_answers_records_and_hands_out_addresses_as_told() {
    let (_runtime, lab, _log, _room) = network(data!({"cidr": "10.60.0.0/24"}));
    for (node, kind) in [("pc1", "pc"), ("srv1", "server")] {
        ok(lab
            .act("light.add", data!({"node": node, "kind": kind, "at": "gw"}))
            .unwrap());
    }
    let _vm = Guest::cable(&lab, "vm1");
    let code = |capability: &str, arguments: Data| {
        lab.act(capability, arguments).unwrap().reason.unwrap().code
    };
    let address = |node: &str| {
        lab.refresh().unwrap().facts["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["node"] == node)
            .unwrap()["address"]
            .clone()
    };
    assert_eq!(settles(&lab, "pc1", "10.60.0.10")["dns"], "10.60.0.1");

    // Records, under a domain of the network's own.
    ok(lab.act("dns.domain", data!({"domain": "lab"})).unwrap());
    for (name, kind, value) in [
        ("www.lab", "CNAME", "srv1.lab"),
        ("lab", "MX", "correio.lab"),
        ("lab", "TXT", "v=spf1 mx -all"),
        ("srv1.lab", "AAAA", "fd00::5"),
    ] {
        ok(lab
            .act(
                "dns.record",
                data!({"name": name, "type": kind, "value": value}),
            )
            .unwrap());
    }
    let said = run(&lab, "pc1", "nslookup www.lab");
    assert_eq!(said[0]["code"], "dns.alias", "{said:?}");
    assert_eq!(said[0]["to"], "srv1.lab");
    assert_eq!(said[1]["address"], "10.60.0.11", "{said:?}");
    // The bare name is the same name.
    let said = run(&lab, "pc1", "nslookup www");
    assert!(
        said.iter().any(|l| l["address"] == "10.60.0.11"),
        "{said:?}"
    );
    let said = run(&lab, "pc1", "nslookup -type=mx lab");
    assert_eq!(said[0]["value"], "10 correio.lab", "{said:?}");
    let said = run(&lab, "pc1", "dig lab txt");
    assert_eq!(said[0]["value"], "v=spf1 mx -all", "{said:?}");
    let said = run(&lab, "pc1", "host -t aaaa srv1.lab");
    assert_eq!(said[0]["address"], "fd00::5", "{said:?}");
    assert_eq!(run(&lab, "pc1", "nslookup nada.lab")[0]["code"], "dns.none");
    ok(lab
        .act(
            "dns.record_remove",
            data!({"name": "lab", "type": "TXT", "value": "v=spf1 mx -all"}),
        )
        .unwrap());
    assert_eq!(run(&lab, "pc1", "dig lab txt")[0]["code"], "dns.none");

    // Another DNS server and a shorter lease, heard on renewing.
    ok(lab
        .act(
            "dhcp.configure",
            data!({"vlan": 1, "dns": "10.60.0.53", "lease": 120}),
        )
        .unwrap());
    let said = run(&lab, "pc1", "ipconfig /renew");
    assert_eq!(said[0]["dns"], "10.60.0.53", "{said:?}");
    assert_eq!(
        ok(lab.act("network.attach", data!({"node": "vm1"})).unwrap())["dns"][0],
        "10.60.0.53"
    );

    // Fewer addresses handed out: who takes theirs from the router moves
    // in; the virtual machine is reserved and stays, until let go.
    ok(lab
        .act(
            "dhcp.configure",
            data!({"vlan": 1, "first": 50, "last": 60}),
        )
        .unwrap());
    assert_eq!(address("pc1"), "10.60.0.50");
    assert_eq!(address("srv1"), "10.60.0.51");
    assert_eq!(address("vm1"), "10.60.0.12");
    assert_eq!(settles(&lab, "pc1", "10.60.0.50")["address"], "10.60.0.50");
    ok(lab
        .act("node.reserve", data!({"node": "vm1", "on": false}))
        .unwrap());
    assert_eq!(address("vm1"), "10.60.0.52");
    let facts = lab.refresh().unwrap().facts;
    let main = &facts["routers"][0]["networks"][0];
    assert_eq!(
        main["pool"],
        serde_json::json!(["10.60.0.50", "10.60.0.60"])
    );
    assert_eq!(main["dhcp"]["lease"], 120);

    // Off: nobody answers.
    ok(lab
        .act("dhcp.configure", data!({"vlan": 1, "on": false}))
        .unwrap());
    assert_eq!(run(&lab, "pc1", "ipconfig /renew")[0]["code"], "dhcp.none");
    ok(lab
        .act("dhcp.configure", data!({"vlan": 1, "on": true}))
        .unwrap());

    // What is refused.
    assert_eq!(
        code("dhcp.configure", data!({"vlan": 1, "first": 1})),
        "network.bad_pool"
    );
    assert_eq!(
        code("dhcp.configure", data!({"vlan": 1, "lease": 5})),
        "network.bad_lease"
    );
    assert_eq!(
        code(
            "dns.record",
            data!({"name": "x.lab", "type": "SRV", "value": "y"})
        ),
        "network.bad_rule"
    );
    assert_eq!(
        code(
            "dns.record",
            data!({"name": "x.lab", "type": "CNAME", "value": "x.lab"})
        ),
        "network.bad_dns_name"
    );

    // A name the Server published is shown as its, and goes with it: the
    // file the network is written to has the person's names only.
    ok(lab
        .act(
            "dns.add",
            data!({"name": "portal.lab", "address": "10.60.0.20", "from": "sbx-server"}),
        )
        .unwrap());
    ok(lab
        .act(
            "dns.add",
            data!({"name": "files.lab", "address": "10.60.0.9"}),
        )
        .unwrap());
    let facts = lab.refresh().unwrap().facts;
    assert_eq!(facts["dns_from"]["portal.lab"], "sbx-server");
    let state = ok(lab.act("network.export", Data::new()).unwrap())["state"].clone();
    assert!(state["dns"].get("portal.lab").is_none());
    assert_eq!(state["dns"]["files.lab"], "10.60.0.9");
    assert_eq!(state["domain"], "lab");

    // And it all comes back in another network made from that file.
    let (_runtime2, copy, _log2, _room2) = network(data!({"cidr": "10.60.0.0/24"}));
    ok(copy.act("network.import", data!({"state": state})).unwrap());
    let facts = copy.refresh().unwrap().facts;
    assert_eq!(facts["domain"], "lab");
    assert_eq!(facts["records"].as_array().unwrap().len(), 3);
    assert_eq!(facts["routers"][0]["networks"][0]["dhcp"]["first"], 50);
    assert_eq!(settles(&copy, "pc1", "10.60.0.50")["dns"], "10.60.0.53");
}
