//! A Server's internal DNS, on a real network's switch, asked by a guest
//! that is only a socket.
//!
//! The Server takes a cable like any guest, answers ARP and UDP, and is
//! policed by the switch like any node. Lives here because only the API may
//! see both the network and server crates (`deny.toml`).
#![cfg(unix)]

use std::net::Ipv4Addr;
use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use beatbox_engine::{
    ActionOutcome, Data, Recorder, RuntimeParts, Sandbox, SandboxConfig, SandboxRegistry,
    SandboxRuntime, data,
};
use beatbox_network::NetworkSandbox;
use beatbox_network::frame::{self, BROADCAST, Mac};
use beatbox_server::ServerSandbox;
use serde_json::json;

fn ok(outcome: ActionOutcome) -> Data {
    assert!(outcome.ok, "{:?}", outcome.reason);
    outcome.data
}

/// A guest that is nothing but its end of a cable.
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
            .set_read_timeout(Some(Duration::from_millis(300)))
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

    /// The first frame within a second that `wanted` accepts.
    fn receive<T>(&self, wanted: impl Fn(&[u8]) -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut buffer = [0u8; 2048];
        while Instant::now() < deadline {
            if let Ok(size) = self.socket.recv(&mut buffer)
                && let Some(found) = wanted(&buffer[..size])
            {
                return Some(found);
            }
        }
        None
    }

    /// Who has `address`? The MAC that says it does.
    fn arp(&self, address: Ipv4Addr) -> Option<Mac> {
        let mut body = vec![0, 1, 8, 0, 6, 4, 0, 1];
        body.extend_from_slice(&self.mac);
        body.extend_from_slice(&self.address.octets());
        body.extend_from_slice(&[0; 6]);
        body.extend_from_slice(&address.octets());
        self.send(&frame::ethernet(
            BROADCAST,
            self.mac,
            frame::ETHER_ARP,
            &body,
        ));
        self.receive(|bytes| {
            let arp = frame::arp(bytes)?;
            (arp.operation == 2 && arp.sender_ip == address).then_some(arp.sender_mac)
        })
    }

    /// Ask `server` for `name`'s A record: the DNS answer's bytes, if any.
    fn ask(&self, server: Ipv4Addr, server_mac: Mac, name: &str) -> Option<Vec<u8>> {
        let mut query = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            query.push(label.len() as u8);
            query.extend_from_slice(label.as_bytes());
        }
        query.extend_from_slice(&[0, 0, 1, 0, 1]);
        let datagram = frame::udp(40_000, 53, &query);
        let ip = frame::ipv4_packet(self.address, server, frame::UDP, &datagram);
        self.send(&frame::ethernet(
            server_mac,
            self.mac,
            frame::ETHER_IPV4,
            &ip,
        ));
        self.receive(|bytes| {
            let packet = frame::ipv4(bytes)?;
            let flow = packet.flow;
            (flow.source == server && flow.source_port == 53 && flow.destination_port == 40_000)
                .then(|| frame::udp_data(packet.payload).to_vec())
        })
    }
}

struct Lab {
    _runtime: SandboxRuntime,
    network: Sandbox,
    server: Sandbox,
    log: Recorder,
    rooms: Vec<std::path::PathBuf>,
}

impl Drop for Lab {
    /// The sandboxes first, then their rooms: a switch is stopped through a
    /// socket in its room, and taking the room away first leaves its thread
    /// waiting on nothing.
    fn drop(&mut self) {
        let _ = self.server.destroy();
        let _ = self.network.destroy();
        for room in &self.rooms {
            let _ = std::fs::remove_dir_all(room);
        }
    }
}

fn lab() -> Lab {
    let mut registry = SandboxRegistry::default();
    registry.register(NetworkSandbox::spec()).unwrap();
    registry.register(ServerSandbox::spec()).unwrap();
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let log = Recorder::new(1000);
    runtime.events().subscribe("*", log.sink()).unwrap();
    // The clock alone is not unique across two tests on two threads.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let stamp = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let rooms = ["net", "srv"].map(|what| {
        std::env::temp_dir().join(format!("bbs-{what}-{}-{stamp}", std::process::id()))
    });
    let network = runtime
        .create(
            SandboxConfig::new("network")
                .unwrap()
                .named("lab")
                .with_settings(
                    data!({"cidr": "10.20.0.0/24", "workspace": rooms[0].to_string_lossy()}),
                ),
        )
        .unwrap();
    network.start().unwrap();
    let server = runtime
        .create(
            SandboxConfig::new("server")
                .unwrap()
                .named("srv")
                .with_settings(data!({"workspace": rooms[1].to_string_lossy()}))
                .using("network", "lab"),
        )
        .unwrap();
    server.start().unwrap();
    Lab {
        _runtime: runtime,
        network,
        server,
        log,
        rooms: rooms.to_vec(),
    }
}

fn act(sandbox: &Sandbox, capability: &str, arguments: Data) -> ActionOutcome {
    sandbox.act(capability, arguments).unwrap()
}

fn record(lab: &Lab, capability: &str, arguments: serde_json::Value) -> ActionOutcome {
    act(
        &lab.server,
        "service.call",
        data!({"service": "names", "capability": capability, "arguments": arguments}),
    )
}

/// The A record in a DNS answer, and its response code.
fn read_answer(answer: &[u8]) -> (u8, Option<Ipv4Addr>) {
    let code = answer[3] & 0x0f;
    let count = u16::from_be_bytes([answer[6], answer[7]]);
    let address = (count > 0).then(|| {
        let at = answer.len() - 4;
        Ipv4Addr::new(answer[at], answer[at + 1], answer[at + 2], answer[at + 3])
    });
    (code, address)
}

#[test]
fn a_guest_asks_the_servers_dns_and_the_firewall_can_stop_it() {
    let lab = lab();
    ok(act(
        &lab.server,
        "service.create",
        data!({"definition": "dns", "name": "names"}),
    ));
    // A record added while the DNS is stopped is kept for when it starts.
    ok(record(
        &lab,
        "dns.record.add",
        json!({"name": "intranet.lab", "address": "10.20.0.50"}),
    ));

    let started = ok(act(
        &lab.server,
        "service.start",
        data!({"service": "names"}),
    ));
    assert_eq!(started["state"], "running");
    assert_eq!(started["health"], "healthy");

    let facts = lab.server.refresh().unwrap().facts;
    let endpoint = &facts["endpoints"][0];
    assert_eq!(endpoint["transport"], "udp");
    assert_eq!(endpoint["port"], 53);
    let server: Ipv4Addr = endpoint["address"].as_str().unwrap().parse().unwrap();

    let guest = Guest::cable(&lab.network, "client");
    let server_mac = guest
        .arp(server)
        .expect("the server answers ARP for its address");

    let answer = guest
        .ask(server, server_mac, "intranet.lab")
        .expect("an answer");
    assert_eq!(
        read_answer(&answer),
        (0, Some(Ipv4Addr::new(10, 20, 0, 50)))
    );
    let answer = guest
        .ask(server, server_mac, "nowhere.lab")
        .expect("an answer");
    assert_eq!(
        read_answer(&answer).0,
        3,
        "NXDOMAIN for a name it does not have"
    );

    // Policed like any node: a rule on the switch, and the question never
    // reaches it.
    let server_node = lab.server.id();
    ok(act(
        &lab.network,
        "firewall.add",
        data!({"action": "deny", "source": "client", "destination": server_node,
               "protocol": "udp", "port": 53}),
    ));
    assert!(
        guest.ask(server, server_mac, "intranet.lab").is_none(),
        "blocked"
    );
    assert!(
        lab.log.names().iter().any(|name| name == "packet.blocked"),
        "the network said so: {:?}",
        lab.log.names()
    );

    // Stopped, it gives the cable back; its records stay.
    ok(act(
        &lab.server,
        "service.stop",
        data!({"service": "names"}),
    ));
    let refused = act(&lab.network, "network.detach", data!({"node": server_node}));
    assert!(!refused.ok, "already detached");
    let kept = ok(record(&lab, "dns.resolve", json!({"name": "intranet.lab"})));
    assert_eq!(kept["address"], "10.20.0.50");
}

#[test]
fn an_internal_service_needs_a_network_and_udp() {
    let lab = lab();
    let tcp = json!({
        "id": "dns-tcp", "version": "1", "name": "DNS",
        "runtime": {"type": "internal", "implementation": "dns"},
        "interfaces": [{"transport": "tcp", "port": 53}]
    });
    let refused = act(
        &lab.server,
        "service.create",
        data!({"definition": tcp, "name": "x"}),
    );
    assert_eq!(refused.reason.unwrap().code, "server.internal_udp_only");
    ok(act(
        &lab.server,
        "service.create",
        data!({"definition": "dns", "name": "one"}),
    ));
    ok(act(
        &lab.server,
        "service.create",
        data!({"definition": "dns", "name": "two"}),
    ));
    ok(act(&lab.server, "service.start", data!({"service": "one"})));
    let taken = act(&lab.server, "service.start", data!({"service": "two"}));
    assert_eq!(taken.reason.unwrap().code, "server.port_taken");
}

#[test]
fn the_servers_names_reach_every_machine_through_the_networks_dns() {
    let lab = lab();
    let resolve =
        |name: &str| ok(act(&lab.network, "dns.resolve", data!({"name": name})))["address"].clone();
    // Someone's own record on the network: the Server never takes it.
    ok(act(
        &lab.network,
        "dns.add",
        data!({"name": "mail.lab", "address": "10.20.0.77"}),
    ));
    ok(act(
        &lab.server,
        "service.create",
        data!({"definition": "dns", "name": "names"}),
    ));
    for (name, address) in [("intranet.lab", "10.20.0.50"), ("mail.lab", "10.20.0.99")] {
        ok(record(
            &lab,
            "dns.record.add",
            json!({"name": name, "address": address}),
        ));
    }
    assert!(
        resolve("intranet.lab").is_null(),
        "nothing published while stopped"
    );

    ok(act(
        &lab.server,
        "service.start",
        data!({"service": "names"}),
    ));
    assert_eq!(resolve("intranet.lab"), "10.20.0.50");
    assert_eq!(resolve("mail.lab"), "10.20.0.77");
    // The network says whose name it is, and keeps it out of its own file.
    let facts = lab.network.refresh().unwrap().facts;
    assert_eq!(facts["dns_from"]["intranet.lab"], lab.server.id());
    assert!(facts["dns_from"].get("mail.lab").is_none());
    let state = ok(act(&lab.network, "network.export", data!({})))["state"].clone();
    assert!(state["dns"].get("intranet.lab").is_none());
    assert_eq!(state["dns"]["mail.lab"], "10.20.0.77");

    // A machine asks the DNS DHCP gave it -- the gateway -- not the Server.
    let guest = Guest::cable(&lab.network, "client");
    let gateway = Ipv4Addr::new(10, 20, 0, 1);
    let gateway_mac = guest.arp(gateway).expect("the gateway answers ARP");
    let answer = guest
        .ask(gateway, gateway_mac, "intranet.lab")
        .expect("an answer");
    assert_eq!(
        read_answer(&answer),
        (0, Some(Ipv4Addr::new(10, 20, 0, 50)))
    );

    // Changed while running, the network follows.
    ok(record(
        &lab,
        "dns.record.add",
        json!({"name": "files.lab", "address": "10.20.0.60"}),
    ));
    ok(record(
        &lab,
        "dns.record.remove",
        json!({"name": "intranet.lab"}),
    ));
    assert_eq!(resolve("files.lab"), "10.20.0.60");
    assert!(resolve("intranet.lab").is_null());

    // Stopped, it takes back what it added and only that.
    ok(act(
        &lab.server,
        "service.stop",
        data!({"service": "names"}),
    ));
    assert!(resolve("files.lab").is_null());
    assert_eq!(resolve("mail.lab"), "10.20.0.77");
}

/// The network an internal service answers on, stopped, started and deleted
/// under it (#70). Off or gone, nobody can reach it, so it is not said to
/// run; back, it runs again on its own, and answers.
#[test]
fn a_network_going_and_coming_back_is_what_its_dns_says() {
    let lab = lab();
    ok(act(
        &lab.server,
        "service.create",
        data!({"definition": "dns", "name": "names"}),
    ));
    ok(act(
        &lab.server,
        "service.start",
        data!({"service": "names"}),
    ));
    ok(record(
        &lab,
        "dns.record.add",
        json!({"name": "portal.lab", "address": "10.20.0.9"}),
    ));
    let names = |lab: &Lab| {
        let listed = ok(act(
            &lab.server,
            "service.list",
            data!({"check_health": true}),
        ));
        listed["services"][0].clone()
    };

    lab.network.stop().unwrap();
    let off = names(&lab);
    assert_eq!(off["state"], "stopped");
    assert_eq!(off["health"], "unknown");
    assert_eq!(off["endpoints"], json!([]));

    lab.network.start().unwrap();
    let back = names(&lab);
    assert_eq!(back["state"], "running");
    assert_eq!(back["health"], "healthy");
    let server: Ipv4Addr = back["endpoints"][0]["address"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let guest = Guest::cable(&lab.network, "client");
    let mac = guest.arp(server).expect("it answers ARP again");
    let answer = guest.ask(server, mac, "portal.lab").expect("an answer");
    assert_eq!(read_answer(&answer), (0, Some(Ipv4Addr::new(10, 20, 0, 9))));
    drop(guest);

    lab.network.destroy().unwrap();
    let gone = names(&lab);
    assert_eq!(gone["state"], "stopped");
    assert_eq!(gone["endpoints"], json!([]));
    ok(act(
        &lab.server,
        "service.remove",
        data!({"service": "names"}),
    ));
}

/// Stopping a Server stops its internal services, and the runtime's look
/// after each transition must not bring them back (#70). Started again,
/// they run again.
#[test]
fn a_stopped_server_keeps_its_dns_off_the_network() {
    let lab = lab();
    ok(act(
        &lab.server,
        "service.create",
        data!({"definition": "dns", "name": "names"}),
    ));
    ok(act(
        &lab.server,
        "service.start",
        data!({"service": "names"}),
    ));
    let on_network = |lab: &Lab| {
        lab.network.refresh().unwrap().facts["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["node"] == lab.server.id())
    };
    assert!(on_network(&lab));

    lab.server.stop().unwrap();
    lab.server.refresh().unwrap();
    assert!(!on_network(&lab), "a stopped server is off the network");
    let listed = ok(act(&lab.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["state"], "stopped");
    assert!(!on_network(&lab));

    lab.server.start().unwrap();
    let listed = ok(act(&lab.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["state"], "running");
    assert!(on_network(&lab));
}

/// A Server's domain names each running service on the network,
/// `<service>.<domain>`: gone when it stops, moved when the domain changes.
#[test]
fn a_domain_names_each_running_service_on_the_network() {
    let lab = lab();
    let resolve =
        |name: &str| ok(act(&lab.network, "dns.resolve", data!({"name": name})))["address"].clone();
    ok(act(
        &lab.server,
        "service.create",
        data!({"definition": "dns", "name": "names"}),
    ));
    ok(act(
        &lab.server,
        "service.start",
        data!({"service": "names"}),
    ));
    let listed = ok(act(&lab.server, "service.list", Data::new()));
    let address = listed["services"][0]["endpoints"][0]["address"].clone();
    assert!(address.is_string());
    assert!(resolve("names.srv.lab").is_null(), "no domain, no names");

    ok(act(
        &lab.server,
        "server.settings",
        data!({"domain": "srv.lab"}),
    ));
    assert_eq!(resolve("names.srv.lab"), address);
    // The network says whose it is.
    let facts = lab.network.refresh().unwrap().facts;
    assert_eq!(facts["dns_from"]["names.srv.lab"], lab.server.id());

    ok(act(
        &lab.server,
        "server.settings",
        data!({"domain": "escola.lab"}),
    ));
    assert!(resolve("names.srv.lab").is_null());
    assert_eq!(resolve("names.escola.lab"), address);

    ok(act(
        &lab.server,
        "service.stop",
        data!({"service": "names"}),
    ));
    assert!(resolve("names.escola.lab").is_null(), "stopped, unnamed");
    ok(act(
        &lab.server,
        "service.start",
        data!({"service": "names"}),
    ));
    assert_eq!(resolve("names.escola.lab"), address);

    lab.server.stop().unwrap();
    assert!(
        resolve("names.escola.lab").is_null(),
        "a stopped Server names nothing"
    );
    lab.server.start().unwrap();
    assert_eq!(resolve("names.escola.lab"), address);
}
