//! A network, as a sandbox: the same nine methods a host is driven through.

use std::collections::BTreeMap;
#[cfg(unix)]
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use beatbox_engine::runtime::capabilities::{LIFECYCLE_RECONFIGURE, LIFECYCLE_RESET};
use beatbox_engine::{
    Action, ActionSpec, Data, Emitter, Field, Isolation, Kind, Observation, ProviderError,
    ProviderSpec, Reason, SandboxConfig, SandboxIdentity, SandboxProvider, data, reason,
};
use serde_json::{Value, json};

use crate::capture::{self, Capture, Keep, Talk, Tap};
use crate::frame::{self, mac_text, node_mac};
use crate::light::LightConf;
use crate::renumber;
use crate::switch::{
    Baseline, Forward, Limit, NameAction, NameRule, Net, Node, Route, Rule, Service, Side, Traffic,
    Verdict, cable_key, gateway_of, gateway6, overlap, parse_cidr, parse_cidr6, prefix6,
};
use crate::topology::{self, End, Mode, Topology};

/// What a network can do.
pub const CAPABILITIES: [&str; 60] = [
    LIFECYCLE_RESET,
    LIFECYCLE_RECONFIGURE,
    "network.attach",
    "network.detach",
    "network.flows",
    "network.export",
    "network.import",
    "point.save",
    "point.list",
    "point.restore",
    "point.remove",
    "firewall.add",
    "firewall.remove",
    "firewall.name",
    "firewall.name_remove",
    "firewall.default",
    "firewall.move",
    "firewall.answers",
    "firewall.group",
    "firewall.service",
    "capture.start",
    "capture.stop",
    "capture.remove",
    "capture.save",
    "router.internet",
    "network.renumber_plan",
    "device.add",
    "device.remove",
    "device.rename",
    "node.rename",
    "link.add",
    "link.remove",
    "port.configure",
    "node.place",
    "node.address",
    "interface.configure",
    "interface.remove",
    "route.add",
    "route.remove",
    "nat.hide",
    "ipv6.set",
    "nat.add",
    "nat.remove",
    "cable.configure",
    "vlan.name",
    "light.add",
    "light.configure",
    "light.run",
    "light.start",
    "light.result",
    "dns.add",
    "dns.remove",
    "dns.resolve",
    "dns.record",
    "dns.record_remove",
    "dns.domain",
    "dhcp.configure",
    "node.reserve",
    "traffic.configure",
    "traffic.observe",
];

/// What each capability past the lifecycle takes and gives back. The
/// runtime checks the inputs' kinds; `firewall.add`'s ends and `dns.*`'s
/// names are checked again here for what they say.
pub fn actions() -> Vec<ActionSpec> {
    use Kind::{List, Number, Object, Text};
    let need = Field::required;
    let may = Field::optional;
    let traffic = || {
        ["latency_ms", "jitter_ms", "loss_percent", "bandwidth_kbps"]
            .into_iter()
            .map(|key| (key, Number))
            .collect::<Vec<_>>()
    };
    vec![
        ActionSpec::new(
            "network.attach",
            vec![need("node", Text), may("address", Text), may("at", Text)],
            vec![
                need("node", Text),
                need("address", Text),
                need("prefix", Number),
                need("gateway", Text),
                need("dns", List),
                need("mac", Text),
                need("switch", Text),
                need("port", Text),
            ],
        ),
        ActionSpec::new(
            "network.detach",
            vec![need("node", Text)],
            vec![need("node", Text)],
        ),
        ActionSpec::new(
            "network.flows",
            vec![],
            vec![need("flows", List), need("lookups", List)],
        ),
        ActionSpec::new(
            "network.export",
            vec![
                may("file", Kind::Bool),
                may("folder", Text),
                may("name", Text),
            ],
            vec![need("state", Object), may("path", Text)],
        ),
        ActionSpec::new(
            "network.import",
            vec![need("state", Object)],
            vec![
                need("devices", Number),
                need("lights", Number),
                need("rules", Number),
            ],
        ),
        ActionSpec::new(
            "point.save",
            vec![may("name", Text)],
            vec![need("id", Text)],
        ),
        ActionSpec::new("point.list", vec![], vec![need("points", List)]),
        ActionSpec::new(
            "point.restore",
            vec![need("id", Text)],
            vec![
                need("devices", Number),
                need("lights", Number),
                need("rules", Number),
            ],
        ),
        ActionSpec::new(
            "point.remove",
            vec![need("id", Text)],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "firewall.add",
            vec![
                need("action", Text),
                may("source", Text),
                may("destination", Text),
                may("protocol", Text),
                may("port", Number),
                may("port_last", Number),
                may("icmp_type", Number),
                may("ethertype", Number),
                may("state", Text),
                may("service", Text),
                may("limit", Object),
                may("family", Number),
            ],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "firewall.name",
            vec![
                need("action", Text),
                need("name", Text),
                may("source", Text),
                may("to", Text),
            ],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "firewall.name_remove",
            vec![need("id", Text)],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "firewall.remove",
            vec![need("id", Text)],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "firewall.default",
            vec![need("policy", Text)],
            vec![need("policy", Text)],
        ),
        ActionSpec::new(
            "firewall.move",
            vec![need("id", Text), need("index", Number)],
            vec![need("id", Text), need("index", Number)],
        ),
        ActionSpec::new(
            "firewall.answers",
            vec![need("on", Kind::Bool)],
            vec![need("on", Kind::Bool)],
        ),
        ActionSpec::new(
            "firewall.group",
            vec![need("name", Text), need("members", List)],
            vec![need("name", Text)],
        ),
        ActionSpec::new(
            "firewall.service",
            vec![
                need("name", Text),
                may("protocol", Text),
                may("port", Number),
                may("port_last", Number),
            ],
            vec![need("name", Text)],
        ),
        ActionSpec::new(
            "capture.start",
            vec![
                may("node", Text),
                may("cable", Text),
                may("keep", Text),
                may("source", Text),
                may("destination", Text),
                may("protocol", Text),
                may("port", Number),
            ],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "capture.stop",
            vec![need("id", Text)],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "capture.remove",
            vec![need("id", Text)],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "capture.save",
            vec![need("id", Text)],
            vec![need("id", Text), need("path", Text)],
        ),
        ActionSpec::new(
            "ipv6.set",
            vec![
                may("router", Text),
                need("vlan", Number),
                need("on", Kind::Bool),
            ],
            vec![
                need("router", Text),
                need("vlan", Number),
                need("on", Kind::Bool),
            ],
        ),
        ActionSpec::new(
            "nat.hide",
            vec![
                may("router", Text),
                need("vlan", Number),
                need("on", Kind::Bool),
            ],
            vec![
                need("router", Text),
                need("vlan", Number),
                need("on", Kind::Bool),
            ],
        ),
        ActionSpec::new(
            "nat.add",
            vec![
                may("router", Text),
                need("protocol", Text),
                need("port", Number),
                need("to", Text),
                need("to_port", Number),
            ],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "nat.remove",
            vec![may("router", Text), need("id", Text)],
            vec![need("id", Text)],
        ),
        ActionSpec::new(
            "router.internet",
            vec![need("on", Kind::Bool)],
            vec![need("on", Kind::Bool)],
        ),
        ActionSpec::new(
            "network.renumber_plan",
            vec![need("cidr", Text)],
            vec![
                need("from", Text),
                need("to", Text),
                need("gateway", Text),
                need("nodes", List),
                need("names", List),
                need("rules", List),
                need("kept", List),
                need("settings", Object),
            ],
        ),
        ActionSpec::new(
            "device.add",
            vec![need("kind", Text), need("name", Text)],
            vec![need("name", Text)],
        ),
        ActionSpec::new(
            "device.remove",
            vec![need("device", Text)],
            vec![need("device", Text)],
        ),
        ActionSpec::new(
            "device.rename",
            vec![need("device", Text), need("name", Text)],
            vec![need("device", Text), need("name", Text)],
        ),
        ActionSpec::new(
            "node.rename",
            vec![need("node", Text), need("name", Text)],
            vec![need("node", Text), need("name", Text)],
        ),
        ActionSpec::new(
            "link.add",
            vec![need("a", Text), need("b", Text)],
            vec![need("a", Text), need("b", Text)],
        ),
        ActionSpec::new(
            "link.remove",
            vec![need("at", Text)],
            vec![need("at", Text)],
        ),
        ActionSpec::new(
            "port.configure",
            vec![
                need("port", Text),
                need("mode", Text),
                may("vlan", Number),
                may("native", Number),
                may("allowed", List),
            ],
            vec![need("port", Text), need("mode", Object)],
        ),
        ActionSpec::new(
            "node.place",
            vec![need("node", Text), need("at", Text)],
            vec![need("node", Text), need("at", Text)],
        ),
        ActionSpec::new(
            "node.address",
            vec![need("node", Text), need("address", Text)],
            vec![
                need("node", Text),
                need("address", Text),
                need("from", Text),
            ],
        ),
        ActionSpec::new(
            "interface.configure",
            vec![
                need("vlan", Number),
                need("cidr", Text),
                may("router", Text),
                may("address", Text),
            ],
            vec![
                need("router", Text),
                need("vlan", Number),
                need("cidr", Text),
                need("gateway", Text),
                need("moved", List),
            ],
        ),
        ActionSpec::new(
            "interface.remove",
            vec![need("vlan", Number), may("router", Text)],
            vec![need("router", Text), need("vlan", Number)],
        ),
        ActionSpec::new(
            "route.add",
            vec![need("to", Text), need("via", Text), may("router", Text)],
            vec![need("router", Text), need("to", Text), need("via", Text)],
        ),
        ActionSpec::new(
            "route.remove",
            vec![need("to", Text), may("router", Text)],
            vec![need("router", Text), need("to", Text)],
        ),
        ActionSpec::new(
            "cable.configure",
            std::iter::once(need("cable", Text))
                .chain(traffic().into_iter().map(|(key, kind)| may(key, kind)))
                .collect(),
            std::iter::once(need("cable", Text))
                .chain(traffic().into_iter().map(|(key, kind)| need(key, kind)))
                .collect(),
        ),
        ActionSpec::new(
            "vlan.name",
            vec![need("vlan", Number), need("name", Text)],
            vec![need("vlan", Number), need("name", Text)],
        ),
        ActionSpec::new(
            "light.add",
            vec![need("node", Text), may("kind", Text), may("at", Text)],
            vec![
                need("node", Text),
                need("address", Text),
                need("light", Object),
            ],
        ),
        ActionSpec::new(
            "light.configure",
            vec![
                need("node", Text),
                may("dhcp", Kind::Bool),
                may("ping", Kind::Bool),
                may("web", Kind::Bool),
                may("echo", Kind::Bool),
                may("gateway", Text),
                may("dns", Text),
            ],
            vec![need("node", Text), need("light", Object)],
        ),
        ActionSpec::new(
            "light.run",
            vec![need("node", Text), need("line", Text)],
            vec![need("node", Text), need("lines", List)],
        ),
        ActionSpec::new(
            "light.start",
            vec![need("node", Text), need("line", Text)],
            vec![need("node", Text), need("job", Number)],
        ),
        ActionSpec::new(
            "light.result",
            vec![need("job", Number)],
            vec![
                need("job", Number),
                need("done", Kind::Bool),
                need("lines", List),
            ],
        ),
        ActionSpec::new(
            "dns.add",
            vec![need("name", Text), need("address", Text), may("from", Text)],
            vec![need("name", Text), need("address", Text)],
        ),
        ActionSpec::new(
            "dns.record",
            vec![
                need("name", Text),
                need("type", Text),
                need("value", Text),
                may("priority", Number),
            ],
            vec![need("name", Text), need("type", Text), need("value", Text)],
        ),
        ActionSpec::new(
            "dns.record_remove",
            vec![need("name", Text), need("type", Text), need("value", Text)],
            vec![need("name", Text)],
        ),
        ActionSpec::new(
            "dns.domain",
            vec![need("domain", Text)],
            vec![need("domain", Text)],
        ),
        ActionSpec::new(
            "dhcp.configure",
            vec![
                may("router", Text),
                need("vlan", Number),
                may("on", Kind::Bool),
                may("first", Number),
                may("last", Number),
                may("lease", Number),
                may("dns", Text),
            ],
            vec![
                need("router", Text),
                need("vlan", Number),
                need("on", Kind::Bool),
                need("first", Number),
                may("last", Number),
                need("lease", Number),
                may("dns", Text),
            ],
        ),
        ActionSpec::new(
            "node.reserve",
            vec![need("node", Text), need("on", Kind::Bool)],
            vec![need("node", Text), need("on", Kind::Bool)],
        ),
        ActionSpec::new(
            "dns.remove",
            vec![need("name", Text)],
            vec![need("name", Text)],
        ),
        ActionSpec::new(
            "dns.resolve",
            vec![need("name", Text)],
            vec![need("name", Text), may("address", Text)],
        ),
        ActionSpec::new(
            "traffic.configure",
            traffic()
                .into_iter()
                .map(|(key, kind)| may(key, kind))
                .collect(),
            traffic()
                .into_iter()
                .map(|(key, kind)| need(key, kind))
                .collect(),
        ),
        ActionSpec::new(
            "traffic.observe",
            vec![],
            vec![need("counters", Object), need("traffic", Object)],
        ),
    ]
}

/// Where a network lives when nobody said: sockets only, a few bytes, so
/// `/tmp` costs nothing here the way it did for a guest's disk.
fn default_room(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("beatbox-{id}"))
}

/// A UNIX socket's path has to fit in `sun_path`.
const PATH_MAX: usize = 107;

/// A network: a switch in user space, and whatever is cabled to it.
///
/// Configured through `settings`:
///
/// | | |
/// |---|---|
/// | `cidr` | its addresses. Default `10.10.10.0/24`; the first is the gateway. |
/// | `firewall` | `{default: allow\|deny, rules: [{action, source, destination, protocol, port}]}` |
/// | `dns` | `{name: address}`, on top of every node's own name. |
/// | `traffic` | `{latency_ms, jitter_ms, loss_percent, bandwidth_kbps}` |
/// | `workspace` | where its sockets live. |
///
/// What the settings say is also what `lifecycle.reset` goes back to.
pub struct NetworkSandbox {
    identity: SandboxIdentity,
    config: SandboxConfig,
    emitter: Emitter,
    room: PathBuf,
    net: Option<Arc<Mutex<Net>>>,
    #[cfg(unix)]
    switch: Option<crate::switch::Switch>,
    /// The light machines running while the switch does.
    #[cfg(unix)]
    lights: BTreeMap<String, crate::light::Light>,
    /// Terminal lines handed to light machines, by job: who runs it, from
    /// which address, and where its answer will come.
    #[cfg(unix)]
    jobs: BTreeMap<u64, Job>,
    #[cfg(unix)]
    next_job: u64,
}

/// A terminal line a light machine is running (`light.start`).
#[cfg(unix)]
struct Job {
    from: Ipv4Addr,
    since: std::time::Instant,
    answer: std::sync::mpsc::Receiver<Vec<Value>>,
}

impl NetworkSandbox {
    /// `network`, version 1.
    ///
    /// Isolation `none`, honestly: a network is a boundary *between* sandboxes
    /// -- nothing runs inside it -- and it runs inside BeatBox's own process.
    pub fn spec() -> ProviderSpec {
        ProviderSpec::new(
            "network",
            "1",
            &CAPABILITIES,
            Isolation::None,
            |identity, config, emitter| Box::new(Self::new(identity, config, emitter)),
        )
        .with_actions(&actions())
    }

    fn new(identity: SandboxIdentity, config: SandboxConfig, emitter: Emitter) -> Self {
        let room = config
            .setting("workspace")
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty())
            .map_or_else(|| default_room(&identity.id), PathBuf::from);
        Self {
            identity,
            config,
            emitter,
            room,
            net: None,
            #[cfg(unix)]
            switch: None,
            #[cfg(unix)]
            lights: BTreeMap::new(),
            #[cfg(unix)]
            jobs: BTreeMap::new(),
            #[cfg(unix)]
            next_job: 0,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.room.join("net.json")
    }

    /// The network as it was left: its settings, then whatever
    /// `net.json` says changed since -- cables, rules, names, traffic. A
    /// sandbox brought back after `beatboxd` was restarted has no `Net` yet,
    /// and its guests are still holding their cables.
    fn load(&mut self) -> Result<(), ProviderError> {
        if self.net.is_some() {
            return Ok(());
        }
        let net = match std::fs::read(self.state_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        {
            Some(saved) => self.rebuilt(&saved, false)?,
            None => Net::new(baseline(&self.config.settings).map_err(refused)?),
        };
        self.net = Some(Arc::new(Mutex::new(net)));
        Ok(())
    }

    /// The network its settings make, changed as `saved` says: `net.json`,
    /// or a file someone `imported` -- whose rules must all read, and whose
    /// virtual machines and captures stay behind (it holds neither).
    /// Always in this network's own range.
    fn rebuilt(&self, saved: &Value, imported: bool) -> Result<Net, ProviderError> {
        let mut net = Net::new(baseline(&self.config.settings).map_err(refused)?);
        {
            let mut settings = saved.as_object().cloned().unwrap_or_default();
            settings.insert("cidr".into(), json!(cidr_text(&net.baseline)));
            match baseline(&settings) {
                Ok(live) => net = Net::restored(net.baseline, live),
                Err(reason) if imported => return Err(refused(reason)),
                Err(_) => {}
            }
            net.topology = Topology::from_value(&saved["topology"]);
            for (vlan, name) in saved["vlan_names"].as_object().into_iter().flatten() {
                if let (Ok(vlan), Some(name)) = (vlan.parse::<u16>(), name.as_str()) {
                    net.vlan_names.insert(vlan, name.to_owned());
                }
            }
            // Captures kept on disk; one recording when the daemon stopped
            // ends there.
            for raw in saved["captures"]
                .as_array()
                .filter(|_| !imported)
                .into_iter()
                .flatten()
            {
                let id = raw["id"].as_str().unwrap_or_default().to_owned();
                let path = self.room.join("captures").join(format!("{id}.pcap"));
                let tap = match (raw["node"].as_str(), raw["a"].as_str(), raw["b"].as_str()) {
                    (Some(node), _, _) => Some(Tap::Node(node.to_owned())),
                    (None, Some(a), Some(b)) => end_at(&net, a)
                        .zip(end_at(&net, b))
                        .map(|(a, b)| Tap::Link(cable_key(a, b))),
                    _ => None,
                };
                // A file with only its header recorded nothing.
                let (Some(tap), Some(meta)) =
                    (tap, std::fs::metadata(&path).ok().filter(|m| m.len() > 24))
                else {
                    continue;
                };
                let started = raw["started_ms"].as_u64().unwrap_or_default();
                net.captures.push(Capture::kept(
                    id,
                    tap,
                    Keep::named(raw["keep"].as_str().unwrap_or_default()).unwrap_or_default(),
                    path,
                    meta.len(),
                    raw["frames"].as_u64().unwrap_or_default(),
                    started,
                    raw["ended_ms"].as_u64().unwrap_or(started),
                    raw["full"].as_bool().unwrap_or(false),
                ));
            }
            for cable in saved["cable_traffic"].as_array().into_iter().flatten() {
                let Ok(traffic) = traffic(cable) else {
                    continue;
                };
                if let Some(node) = cable["node"].as_str() {
                    net.node_traffic.insert(node.to_owned(), traffic);
                } else if let (Some(a), Some(b)) = (
                    cable["a"].as_str().and_then(|t| end_at(&net, t)),
                    cable["b"].as_str().and_then(|t| end_at(&net, t)),
                ) {
                    net.link_traffic.insert(cable_key(a, b), traffic);
                }
            }
            // The main router's other networks, as files from step 2 wrote them.
            for interface in saved["interfaces"].as_array().into_iter().flatten() {
                let vlan = interface["vlan"]
                    .as_u64()
                    .and_then(|v| u16::try_from(v).ok());
                let range = interface["cidr"].as_str().and_then(parse_cidr);
                if let (Some(vlan), Some(range)) = (vlan, range) {
                    let _ = net.set_interface(vlan, Some(range));
                }
            }
            // Every router's networks and routes.
            for router in saved["routers"].as_array().into_iter().flatten() {
                let Some(device) = router["name"].as_str().and_then(|n| net.topology.find(n))
                else {
                    continue;
                };
                for network in router["networks"].as_array().into_iter().flatten() {
                    let vlan = network["vlan"].as_u64().and_then(|v| u16::try_from(v).ok());
                    let range = network["cidr"].as_str().and_then(parse_cidr);
                    let address = network["address"].as_str().and_then(|a| a.parse().ok());
                    if let (Some(vlan), Some(range), Some(address)) = (vlan, range, address)
                        && !(Some(device) == net.topology.router() && vlan == 1)
                    {
                        let _ = net.set_network(device, vlan, Some(Side { range, address }));
                    }
                }
                for network in router["networks"].as_array().into_iter().flatten() {
                    let vlan = network["vlan"].as_u64().and_then(|v| u16::try_from(v).ok());
                    let (Some(vlan), Some(raw)) = (vlan, network.get("dhcp")) else {
                        continue;
                    };
                    let done = dhcp_of(raw).and_then(|dhcp| net.set_dhcp(device, vlan, dhcp));
                    if let (Err(reason), true) = (done, imported) {
                        return Err(refused(reason));
                    }
                }
                for route in router["routes"].as_array().into_iter().flatten() {
                    let to = route["to"].as_str().and_then(parse_cidr);
                    let via = route["via"].as_str().and_then(|a| a.parse().ok());
                    if let (Some(to), Some(via)) = (to, via) {
                        let _ = net.add_route(device, Route { to, via });
                    }
                }
            }
            for node in saved["nodes"].as_array().into_iter().flatten() {
                let (Some(name), Some(address)) = (
                    node["node"].as_str().filter(|name| node_name(name)),
                    node["address"]
                        .as_str()
                        .and_then(|a| a.parse::<Ipv4Addr>().ok()),
                ) else {
                    continue;
                };
                if imported && !node["light"].is_object() {
                    continue;
                }
                // The MAC a machine was given stays its own when its address
                // changes (`renumber`): the machine holds it in its cable.
                // Older files wrote none, and it was always the address's.
                let mac = node["mac"]
                    .as_str()
                    .and_then(frame::parse_mac)
                    .unwrap_or_else(|| node_mac(address));
                // Where it was plugged in; a file from before devices had
                // every machine on the router's ports, in order.
                let at = node["at"]
                    .as_str()
                    .and_then(|text| net.topology.parse_end(text))
                    .and_then(|(device, port)| {
                        Some(End {
                            device,
                            port: port?,
                        })
                    })
                    .unwrap_or_else(|| End {
                        device: 0,
                        port: net.free_port(0),
                    });
                if node["light"].is_object() {
                    net.lights
                        .insert(name.to_owned(), LightConf::from_value(&node["light"]));
                }
                // A file from before reservations: what was not a light
                // machine was a virtual machine, and those are reserved.
                if node["reserved"]
                    .as_bool()
                    .unwrap_or(!node["light"].is_object())
                {
                    net.reserved.insert(name.to_owned());
                }
                net.nodes.push(Node {
                    name: name.to_owned(),
                    address,
                    mac,
                    port: self.room.join(format!("{name}.sock")),
                    at,
                });
            }
        }
        // NAT last: a forward leads to a machine, and the machines are
        // back only now.
        {
            for router in saved["routers"].as_array().into_iter().flatten() {
                let Some(device) = router["name"].as_str().and_then(|n| net.topology.find(n))
                else {
                    continue;
                };
                for vlan in router["hidden"].as_array().into_iter().flatten() {
                    if let Some(vlan) = vlan.as_u64().and_then(|v| u16::try_from(v).ok()) {
                        let _ = net.set_hidden(device, vlan, true);
                    }
                }
                for vlan in router["ipv6"].as_array().into_iter().flatten() {
                    if let Some(vlan) = vlan.as_u64().and_then(|v| u16::try_from(v).ok()) {
                        let _ = net.set_ipv6(device, vlan, true);
                    }
                }
                for raw in router["forwards"].as_array().into_iter().flatten() {
                    if let Ok(forward) = forward_of(raw) {
                        let _ = net.add_forward(device, forward);
                    }
                }
            }
        }
        Ok(net)
    }

    /// Write down what changed, so a restarted daemon finds it (`load`).
    fn save(&self) {
        let Ok(net) = self.net() else {
            return;
        };
        let mut state = kept(&net, &self.room);
        state.insert(
            "captures".into(),
            facts(&net, &self.room)["captures"].take(),
        );
        drop(net);
        if let Err(error) = std::fs::write(self.state_path(), Value::Object(state).to_string()) {
            log::warn!("{} could not write its state: {error}", self.identity.id);
        }
    }

    fn net(&self) -> Result<MutexGuard<'_, Net>, ProviderError> {
        let net = self
            .net
            .as_ref()
            .ok_or_else(|| ProviderError::new(reason!("engine.not_running")))?;
        Ok(net.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn switch_path(&self) -> PathBuf {
        self.room.join("switch.sock")
    }

    /// What light machine `name` starts from now, if it is one.
    #[cfg(unix)]
    fn light_start(&self, net: &Net, name: &str) -> Option<crate::light::Start> {
        let conf = net.lights.get(name)?.clone();
        let index = net.nodes.iter().position(|n| n.name == name)?;
        let node = &net.nodes[index];
        let (range, gateway) = net.serving(index).map_or(
            ((net.live.network, net.live.prefix), net.live.gateway()),
            |(_, _, side)| (side.range, side.address),
        );
        Some(crate::light::Start {
            name: name.to_owned(),
            mac: node.mac,
            port: node.port.clone(),
            switch: self.switch_path(),
            address: node.address,
            prefix: range.1,
            gateway,
            dns: gateway,
            conf,
        })
    }

    /// The light machines as the network says they are: running while its
    /// switch is, each started again when what it starts from changed.
    fn sync_lights(&mut self) {
        #[cfg(unix)]
        {
            let wanted: BTreeMap<String, crate::light::Start> = match (&self.switch, self.net()) {
                (Some(_), Ok(net)) => net
                    .lights
                    .keys()
                    .filter_map(|name| Some((name.clone(), self.light_start(&net, name)?)))
                    .collect(),
                _ => BTreeMap::new(),
            };
            let stale: Vec<String> = self
                .lights
                .iter()
                .filter(|(name, light)| wanted.get(*name) != Some(&light.start))
                .map(|(name, _)| name.clone())
                .collect();
            for name in stale {
                if let Some(light) = self.lights.remove(&name) {
                    light.stop();
                }
            }
            for (name, start) in wanted {
                if self.lights.contains_key(&name) {
                    continue;
                }
                match crate::light::Light::spawn(start) {
                    Ok(light) => {
                        self.lights.insert(name, light);
                    }
                    Err(error) => {
                        log::warn!("{} could not start {name}: {error}", self.identity.id)
                    }
                }
            }
        }
    }

    /// One line in light machine `name`'s terminal, and what it said back.
    /// When nobody answered, the network says why it thinks so: a firewall
    /// rule that refused it, or nothing at that address.
    fn run_light(&mut self, name: &str, line: &str) -> Result<Data, ProviderError> {
        #[cfg(unix)]
        {
            let Some(light) = self.lights.get(name) else {
                return Err(refused(reason!("network.light_off", node = name)));
            };
            let lines = light.run(line, std::time::Duration::from_secs(15));
            let from = light.start.address;
            let lines = self.explain(lines, from)?;
            Ok(data!({"node": name, "lines": lines}))
        }
        #[cfg(not(unix))]
        {
            let _ = line;
            Err(refused(reason!("network.light_off", node = name)))
        }
    }

    /// Start one terminal line on light machine `name`; its answer is
    /// fetched with `light.result`, so the network is free meanwhile.
    fn start_light(&mut self, name: &str, line: &str) -> Result<Data, ProviderError> {
        #[cfg(unix)]
        {
            let answer = self
                .lights
                .get(name)
                .and_then(|light| Some((light.ask(line)?, light.start.address)));
            let Some((answer, from)) = answer else {
                return Err(refused(reason!("network.light_off", node = name)));
            };
            // Answers nobody came back for are let go after a while.
            self.jobs
                .retain(|_, job| job.since.elapsed() < std::time::Duration::from_secs(120));
            self.next_job += 1;
            let job = self.next_job;
            self.jobs.insert(
                job,
                Job {
                    from,
                    since: std::time::Instant::now(),
                    answer,
                },
            );
            Ok(data!({"node": name, "job": job}))
        }
        #[cfg(not(unix))]
        {
            let _ = line;
            Err(refused(reason!("network.light_off", node = name)))
        }
    }

    /// The answer to a `light.start`, when there is one.
    fn light_result(&mut self, job: u64) -> Result<Data, ProviderError> {
        #[cfg(unix)]
        {
            use std::sync::mpsc::TryRecvError;
            let Some(waiting) = self.jobs.get(&job) else {
                return Err(refused(reason!("network.no_such_job", job = job)));
            };
            let lines = match waiting.answer.try_recv() {
                Ok(lines) => lines,
                Err(TryRecvError::Empty) => {
                    return Ok(data!({"job": job, "done": false, "lines": []}));
                }
                Err(TryRecvError::Disconnected) => vec![json!({"code": "off"})],
            };
            let from = waiting.from;
            self.jobs.remove(&job);
            let lines = self.explain(lines, from)?;
            Ok(data!({"job": job, "done": true, "lines": lines}))
        }
        #[cfg(not(unix))]
        {
            Err(refused(reason!("network.no_such_job", job = job)))
        }
    }

    /// A light machine's answer, with the network's word on why nobody
    /// answered when nobody did: a firewall rule, nobody at that address, or
    /// let through and silent.
    #[cfg(unix)]
    fn explain(&self, mut lines: Vec<Value>, from: Ipv4Addr) -> Result<Vec<Value>, ProviderError> {
        {
            // Some of many tries cut by a rule's limit: which rule, and how much.
            let cut = lines.iter().find(|l| {
                l["code"] == "ssh.many"
                    && l["refused"].as_u64().unwrap_or(0) + l["silent"].as_u64().unwrap_or(0) > 0
            });
            if let Some(to) = cut
                .and_then(|l| l["to"].as_str())
                .and_then(|a| a.parse::<IpAddr>().ok())
            {
                let net = self.net()?;
                if let Some(rule) = net.limited(source(&net, from, to), to, frame::TCP)
                    && let Some(limit) = rule.limit
                {
                    let per = match limit.per_ms {
                        1_000 => "second",
                        3_600_000 => "hour",
                        _ => "minute",
                    };
                    lines.push(json!({"code": "why.limit", "rule": rule.id, "count": limit.count, "per": per}));
                    return Ok(lines);
                }
            }
            // A name a rule hides: which rule.
            if let Some(name) = lines
                .iter()
                .find(|l| l["code"] == "dns.none")
                .and_then(|l| l["name"].as_str())
            {
                let net = self.net()?;
                if let Some(rule) = net
                    .answers_name(name, from.into())
                    .filter(|rule| rule.action == NameAction::Hide)
                {
                    lines.push(json!({"code": "why.name", "rule": rule.id, "name": rule.name}));
                }
                return Ok(lines);
            }
            // Nobody answered a ping or a traceroute (ICMP), or a page or a
            // connection (TCP).
            let silent = lines.iter().find_map(|l| match l["code"].as_str() {
                Some("ping.timeout" | "trace.silent") => Some(frame::ICMP),
                Some("curl.timeout" | "curl.reset" | "ssh.silent") => Some(frame::TCP),
                _ => None,
            });
            let to = lines.iter().find_map(|l| {
                l["to"]
                    .as_str()
                    .or(l["address"].as_str())
                    .and_then(|a| a.parse::<IpAddr>().ok())
            });
            if let (Some(protocol), Some(to)) = (silent, to) {
                let net = self.net()?;
                let protocol = if to.is_ipv6() && protocol == frame::ICMP {
                    frame::ICMPV6
                } else {
                    protocol
                };
                lines.push(match net.refused(source(&net, from, to), to, protocol) {
                    Some(Some(rule)) if rule.starts_with('n') => {
                        let name = net
                            .live
                            .names
                            .iter()
                            .find(|n| n.id == rule)
                            .map(|n| n.name.clone());
                        json!({"code": "why.name", "rule": rule, "name": name})
                    }
                    Some(rule) => json!({"code": "why.firewall", "rule": rule}),
                    None if net.holder(to).is_none()
                        && !net.is_router(to)
                        && !matches!(to, IpAddr::V4(to) if net.outside(to)) =>
                    {
                        json!({"code": "why.nobody", "address": to.to_string()})
                    }
                    None => json!({"code": "why.silent"}),
                });
            }
            Ok(lines)
        }
    }

    fn emit(&self, name: &str, data: Data) {
        if let Err(error) = self.emitter.emit(name, data) {
            log::warn!("{} could not say {name}: {error}", self.identity.id);
        }
    }

    fn attach(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let argument = |key: &str| {
            action
                .argument(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let name = text(action, "node");
        let done = self.attach_node(name.clone(), argument("address"), argument("at"))?;
        // A virtual machine or the Server's: other places show its address,
        // so it stays its own when what the router hands out changes.
        self.net()?.reserved.insert(name);
        Ok(done)
    }

    /// Cable node `name` in: at `at` (a device or one of its ports) or the
    /// first switch, with `address` or the first free one.
    fn attach_node(
        &mut self,
        name: String,
        address: Option<String>,
        at: Option<String>,
    ) -> Result<Data, ProviderError> {
        if !node_name(&name) {
            return Err(refused(reason!("network.bad_node", node = name)));
        }
        let switch = self.switch_path();
        let room = self.room.clone();
        let mut net = self.net()?;
        if let Some(node) = net.node(&name) {
            return Ok(attachment(&net, node, &switch));
        }
        let address = match address.as_deref() {
            Some(text) => {
                let address: Ipv4Addr = text
                    .parse()
                    .map_err(|_| refused(reason!("network.bad_address", address = text)))?;
                let live = &net.live;
                if !live.contains(address)
                    || address == live.network
                    || address == live.gateway()
                    || address == live.broadcast()
                {
                    return Err(refused(reason!(
                        "network.address_outside",
                        address = address,
                        cidr = cidr_text(live),
                    )));
                }
                if net.nodes.iter().any(|node| node.address == address) {
                    return Err(refused(reason!("network.address_taken", address = address)));
                }
                address
            }
            None => net
                .free_address()
                .ok_or_else(|| refused(reason!("network.full", cidr = cidr_text(&net.live))))?,
        };
        let port = room.join(format!("{name}.sock"));
        if port.as_os_str().len() > PATH_MAX {
            return Err(refused(reason!(
                "network.path_too_long",
                path = port.display()
            )));
        }
        let at = match at.as_deref() {
            Some(text) => {
                let at = port_named(&net, text)?;
                let taken =
                    net.nodes.iter().any(|n| n.at == at) || net.topology.across(at).is_some();
                if taken {
                    return Err(refused(reason!("network.port_taken")));
                }
                at
            }
            None => first_place(&net),
        };
        let node = Node {
            name,
            address,
            mac: node_mac(address),
            port,
            at,
        };
        net.nodes.push(node);
        // Into the network of the VLAN its port reaches the router in, and
        // a MAC read off the address it ends up with -- unless a machine
        // that moved since still carries that one (MACs stay with a machine).
        net.readdress();
        let last = net.nodes.len() - 1;
        let mut mac = node_mac(net.nodes[last].address);
        while net.nodes[..last].iter().any(|n| n.mac == mac) {
            mac[1] = mac[1].wrapping_add(1);
        }
        net.nodes[last].mac = mac;
        let answer = attachment(&net, net.nodes.last().expect("just pushed"), &switch);
        drop(net);
        self.emit(
            "network.attached",
            data!({"node": answer["node"], "address": answer["address"], "mac": answer["mac"]}),
        );
        Ok(answer)
    }

    fn detach(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let name = text(action, "node");
        let mut net = self.net()?;
        let Some(at) = net.nodes.iter().position(|node| node.name == name) else {
            return Err(refused(reason!("network.no_such_node", node = name)));
        };
        let node = net.nodes.remove(at);
        net.node_traffic.remove(&node.name);
        net.lights.remove(&node.name);
        net.reserved.remove(&node.name);
        net.leases.remove(&node.name);
        for members in net.live.groups.values_mut() {
            members.retain(|member| *member != node.name);
        }
        drop(net);
        let _ = std::fs::remove_file(&node.port);
        self.emit("network.detached", data!({"node": node.name}));
        Ok(data!({"node": node.name}))
    }
}

impl SandboxProvider for NetworkSandbox {
    fn create(&mut self) -> Result<(), ProviderError> {
        let baseline = baseline(&self.config.settings).map_err(refused)?;
        make_room(&self.room)
            .map_err(|error| refused(reason!("network.could_not_start", detail = error)))?;
        if self.switch_path().as_os_str().len() > PATH_MAX {
            return Err(refused(reason!(
                "network.path_too_long",
                path = self.switch_path().display()
            )));
        }
        self.net = Some(Arc::new(Mutex::new(Net::new(baseline))));
        Ok(())
    }

    #[cfg(unix)]
    fn start(&mut self) -> Result<(), ProviderError> {
        self.load()?;
        let net = Arc::clone(
            self.net
                .as_ref()
                .ok_or_else(|| ProviderError::new(reason!("engine.not_running")))?,
        );
        net.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear_counts();
        // One already picked back up (`observe`) ends first: dropped after
        // the new one binds, it would take the new one's socket with it.
        if let Some(old) = self.switch.take() {
            old.stop();
        }
        let switch =
            crate::switch::Switch::start(&self.switch_path(), net, self.emitter.clone())
                .map_err(|error| refused(reason!("network.could_not_start", detail = error)))?;
        self.switch = Some(switch);
        self.sync_lights();
        Ok(())
    }

    // ponytail: QEMU for Windows has no AF_UNIX datagrams; its `-netdev dgram`
    // takes UDP on 127.0.0.1, which is where a Windows switch would listen.
    #[cfg(not(unix))]
    fn start(&mut self) -> Result<(), ProviderError> {
        Err(refused(reason!("network.not_on_this_system")))
    }

    fn stop(&mut self) -> Result<(), ProviderError> {
        #[cfg(unix)]
        {
            for (_, light) in std::mem::take(&mut self.lights) {
                light.stop();
            }
            if let Some(switch) = self.switch.take() {
                switch.stop();
            }
        }
        Ok(())
    }

    fn destroy(&mut self) -> Result<(), ProviderError> {
        self.stop()?;
        self.net = None;
        match std::fs::remove_dir_all(&self.room) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(ProviderError::new(
                reason!("network.could_not_start", detail = error),
            )),
            _ => Ok(()),
        }
    }

    fn observe(&mut self) -> Observation {
        if self.room.is_dir() {
            let _ = self.load();
        }
        // Reattach: a switch socket still in the room means the daemon that
        // ran this network went away without stopping it (a clean stop takes
        // the socket with it). The guests never noticed; carry their frames
        // again on the same path.
        #[cfg(unix)]
        if self.switch.is_none()
            && self.net.is_some()
            && self.switch_path().exists()
            && let Err(error) = self.start()
        {
            log::warn!(
                "{} could not pick its switch back up: {error}",
                self.identity.id
            );
        }
        #[cfg(unix)]
        let lost = self.switch.as_ref().is_some_and(|switch| !switch.alive());
        #[cfg(not(unix))]
        let lost = false;
        let facts = match &self.net {
            Some(net) => facts(
                &net.lock().unwrap_or_else(PoisonError::into_inner),
                &self.room,
            ),
            None => Data::new(),
        };
        Observation {
            alive: Some(!lost),
            halted: false,
            resources: beatbox_engine::NOTHING,
            facts,
            reason: lost.then(|| reason!("engine.sandbox_lost", type = "network")),
        }
    }

    /// Back to its settings, idle: the engine takes a reset sandbox to
    /// `created`, so the switch stops too. Cables stay (`Net::reset`).
    fn reset(&mut self) -> Result<(), ProviderError> {
        self.stop()?;
        self.load()?;
        self.net()?.reset();
        self.save();
        Ok(())
    }

    /// Only the range may change, and the names and rules that mention it move
    /// with it, exactly as `network.renumber_plan` said: anything else, or a
    /// different move, is refused and the old configuration stays.
    fn reconfigure(&mut self, config: &SandboxConfig) -> Result<(), ProviderError> {
        self.load()?;
        let asked = baseline(&config.settings).map_err(refused)?;
        let to = (asked.network, asked.prefix);
        let plan = {
            let net = self.net()?;
            if to == (net.live.network, net.live.prefix) {
                None
            } else {
                Some(main_plan(&net, to)?)
            }
        };
        let expected = match &plan {
            Some(plan) => plan.settings(&self.config.settings),
            None => self.config.settings.clone(),
        };
        if expected != config.settings {
            return Err(refused(reason!("network.renumber_mismatch")));
        }
        if let Some(plan) = plan {
            let mut net = self.net()?;
            plan.apply(&mut net);
        }
        self.config = config.clone();
        self.save();
        Ok(())
    }

    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        self.load()?;
        let done = self.act(action)?;
        if !matches!(
            action.capability(),
            "network.flows"
                | "network.export"
                | "point.save"
                | "point.list"
                | "point.remove"
                | "dns.resolve"
                | "traffic.observe"
                | "light.run"
                | "light.start"
                | "light.result"
        ) {
            // A port, a cable or a network that moved may have moved a
            // machine into another network: its address follows.
            self.net()?.readdress();
            self.save();
            self.sync_lights();
        }
        Ok(done)
    }
}

impl NetworkSandbox {
    /// A capture on a machine's cable (`node`) or a cable between devices
    /// (`cable`, either of its ends), keeping `keep`, and only one
    /// conversation when `source` and `destination` are given.
    fn start_capture(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let keep = Keep::named(&text(action, "keep")).ok_or_else(|| {
            refused(reason!(
                "network.bad_rule",
                field = "keep",
                value = text(action, "keep")
            ))
        })?;
        let mut net = self.net()?;
        let node = text(action, "node");
        let tap = if node.is_empty() {
            let cable = text(action, "cable");
            let next = end_at(&net, &cable)
                .and_then(|end| Some(cable_key(end, net.topology.across(end)?)))
                .ok_or_else(|| refused(reason!("network.no_cable_there", at = cable)))?;
            Tap::Link(next)
        } else if net.node(&node).is_some() {
            Tap::Node(node.clone())
        } else {
            return Err(refused(reason!("network.no_such_node", node = node)));
        };
        if net.captures.iter().any(|c| c.live() && c.tap == tap) {
            return Err(refused(reason!("network.capture_running")));
        }
        let address = |key: &str| text(action, key).parse::<Ipv4Addr>().ok();
        let talk = match (address("source"), address("destination")) {
            (Some(source), Some(destination)) => Some(Talk {
                source,
                destination,
                protocol: frame::protocol_number(&text(action, "protocol")).unwrap_or(frame::TCP),
                port: action
                    .argument("port")
                    .and_then(Value::as_u64)
                    .and_then(|p| u16::try_from(p).ok())
                    .unwrap_or(0),
            }),
            _ => None,
        };
        let next = net
            .captures
            .iter()
            .filter_map(|c| c.id.strip_prefix('c')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let id = format!("c{next}");
        // Room for one more full capture: the oldest kept ones go first.
        while net.captures.iter().map(|c| c.bytes).sum::<u64>() + capture::MAX_BYTES
            > capture::MAX_TOTAL
        {
            let Some(oldest) = net.captures.iter().position(|c| !c.live()) else {
                break;
            };
            let gone = net.captures.remove(oldest);
            let _ = std::fs::remove_file(&gone.path);
        }
        let path = self.room.join("captures").join(format!("{id}.pcap"));
        let started = Capture::start(id.clone(), tap, keep, talk, path).map_err(|error| {
            refused(reason!("network.capture_failed", error = error.to_string()))
        })?;
        net.captures.push(started);
        Ok(data!({"id": id}))
    }

    /// A copy of capture `id` in the person's Downloads folder, named after
    /// the network and the cable. It may still be recording.
    /// ponytail: the daemon writes it, which holds while it runs on the
    /// person's computer; a remote daemon would hand the bytes to the window.
    fn save_capture(&self, id: &str) -> Result<Data, ProviderError> {
        let net = self.net()?;
        let capture = net
            .captures
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| refused(reason!("network.no_such_capture", id = id)))?;
        capture.flush();
        let what = match &capture.tap {
            Tap::Node(name) => name.clone(),
            Tap::Link((a, b)) => format!(
                "{}--{}",
                net.topology.end_text(*a),
                net.topology.end_text(*b)
            ),
        };
        let folder = downloads();
        let stem = format!("{}-{}-{}", clean(&self.identity.name), clean(&what), id);
        let to = free_file(&folder, &stem, "pcap");
        std::fs::create_dir_all(&folder)
            .and_then(|()| std::fs::copy(&capture.path, &to))
            .map_err(|error| {
                refused(reason!("network.capture_failed", error = error.to_string()))
            })?;
        Ok(data!({"id": id, "path": to.to_string_lossy()}))
    }

    /// This network made what `state` says (`network.export`): its
    /// devices, rules, names and light machines. The virtual machines
    /// cabled to it stay, at the same port when the file has it free;
    /// conversations and leases start over.
    fn replace(&mut self, state: &Value) -> Result<Data, ProviderError> {
        if state["format"] != EXPORT_FORMAT {
            return Err(refused(reason!("network.bad_file")));
        }
        if state["version"].as_u64().is_none_or(|v| v > EXPORT_VERSION) {
            return Err(refused(reason!("network.file_too_new")));
        }
        let mut new = self.rebuilt(state, true)?;
        let mut net = self.net()?;
        for node in &net.nodes {
            if net.lights.contains_key(&node.name) {
                continue;
            }
            // A light machine of a virtual machine's name gives way to it.
            if let Some(at) = new.nodes.iter().position(|n| n.name == node.name) {
                new.nodes.remove(at);
                new.lights.remove(&node.name);
            }
            let at = new
                .topology
                .parse_end(&net.topology.end_text(node.at))
                .and_then(|(device, port)| {
                    Some(End {
                        device,
                        port: port?,
                    })
                })
                .filter(|at| {
                    !new.nodes.iter().any(|n| n.at == *at) && new.topology.across(*at).is_none()
                })
                .unwrap_or_else(|| first_place(&new));
            new.nodes.push(Node { at, ..node.clone() });
        }
        // ponytail: a capture on a cable between devices keeps the ports it
        // was on, whatever is cabled there now.
        new.captures = std::mem::take(&mut net.captures);
        // What was counted so far still happened.
        new.counters = std::mem::take(&mut net.counters);
        new.node_frames = std::mem::take(&mut net.node_frames);
        new.link_frames = std::mem::take(&mut net.link_frames);
        *net = new;
        Ok(data!({
            "devices": net.topology.devices.len(),
            "lights": net.lights.len(),
            "rules": net.live.rules.len(),
        }))
    }

    fn points_path(&self) -> PathBuf {
        self.room.join("points")
    }

    /// The save points, newest first: id, name and when.
    fn points(&self) -> Vec<Value> {
        let mut points: Vec<Value> = std::fs::read_dir(self.points_path())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let id = entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".json")?
                    .to_owned();
                let raw: Value = serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok()?;
                point_id(&id).then(|| json!({"id": id, "name": raw["name"], "at_ms": raw["at_ms"]}))
            })
            .collect();
        points.sort_by_key(|p| std::cmp::Reverse(p["at_ms"].as_u64()));
        points
    }

    fn point_path(&self, action: &Action) -> Result<PathBuf, ProviderError> {
        let id = text(action, "id");
        let path = self.points_path().join(format!("{id}.json"));
        if !point_id(&id) || !path.is_file() {
            return Err(refused(reason!("network.no_such_point", id = id)));
        }
        Ok(path)
    }

    fn act(&mut self, action: &Action) -> Result<Data, ProviderError> {
        match action.capability() {
            "network.export" => {
                let state = Value::Object(exported(&*self.net()?, &self.room));
                if action.argument("file").and_then(Value::as_bool) != Some(true) {
                    return Ok(data!({"state": state}));
                }
                // Where the person chose, under the name they gave; the
                // Downloads folder and the network's name when they did not.
                let folder = match text(action, "folder") {
                    folder if folder.is_empty() => downloads(),
                    folder => PathBuf::from(folder),
                };
                let name = text(action, "name");
                let name = [".bbn", ".beatbox-network.json", ".json"]
                    .iter()
                    .find_map(|ext| name.strip_suffix(ext))
                    .unwrap_or(&name);
                let stem = clean(if name.is_empty() {
                    &self.identity.name
                } else {
                    name
                });
                if !text(action, "folder").is_empty() && !folder.is_dir() {
                    return Err(refused(reason!(
                        "network.no_such_folder",
                        folder = folder.display()
                    )));
                }
                let to = free_file(&folder, &stem, "bbn");
                std::fs::create_dir_all(&folder)
                    .and_then(|()| std::fs::write(&to, format!("{state:#}")))
                    .map_err(|error| {
                        refused(reason!("network.export_failed", error = error.to_string()))
                    })?;
                Ok(data!({"state": state, "path": to.to_string_lossy()}))
            }
            "network.import" => {
                let state = action.argument("state").cloned().unwrap_or_default();
                self.replace(&state)
            }
            "point.save" => {
                let at_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                let id = format!("p{at_ms}");
                let state = Value::Object(exported(&*self.net()?, &self.room));
                let point = json!({"name": text(action, "name"), "at_ms": at_ms, "state": state});
                std::fs::create_dir_all(self.points_path())
                    .and_then(|()| {
                        std::fs::write(
                            self.points_path().join(format!("{id}.json")),
                            point.to_string(),
                        )
                    })
                    .map_err(|error| {
                        refused(reason!("network.export_failed", error = error.to_string()))
                    })?;
                Ok(data!({"id": id}))
            }
            "point.list" => Ok(data!({"points": self.points()})),
            "point.restore" => {
                let path = self.point_path(action)?;
                let point: Value = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default();
                self.replace(&point["state"])
            }
            "point.remove" => {
                let path = self.point_path(action)?;
                let _ = std::fs::remove_file(path);
                Ok(data!({"id": text(action, "id")}))
            }
            "network.attach" => self.attach(action),
            "network.detach" => self.detach(action),
            "device.add" => {
                let name = text(action, "name");
                if !node_name(&name) {
                    return Err(refused(reason!("network.bad_device", device = name)));
                }
                let mut net = self.net()?;
                if net.topology.find(&name).is_some() || net.node(&name).is_some() {
                    return Err(refused(reason!("network.device_taken", device = name)));
                }
                let kind = match text(action, "kind").as_str() {
                    "switch" => topology::Kind::Switch,
                    "router" => topology::Kind::Router,
                    _ => return Err(refused(reason!("network.device_kind"))),
                };
                net.add_device(kind, &name);
                Ok(data!({"name": name}))
            }
            "device.remove" => {
                let name = text(action, "device");
                let mut net = self.net()?;
                let device = net
                    .topology
                    .find(&name)
                    .ok_or_else(|| refused(reason!("network.no_such_device")))?;
                net.remove_device(device).map_err(refused)?;
                Ok(data!({"device": name}))
            }
            "device.rename" => {
                let old = text(action, "device");
                let new = text(action, "name");
                let mut net = self.net()?;
                let device = net
                    .topology
                    .find(&old)
                    .ok_or_else(|| refused(reason!("network.no_such_device")))?;
                if new != old {
                    free_name(&net, &new)?;
                    net.rename_device(device, &new);
                }
                Ok(data!({"device": old, "name": new}))
            }
            "node.rename" => {
                let old = text(action, "node");
                let new = text(action, "name");
                let room = self.room.clone();
                let mut net = self.net()?;
                let index = net
                    .nodes
                    .iter()
                    .position(|n| n.name == old)
                    .ok_or_else(|| refused(reason!("network.no_such_node", node = old)))?;
                // A virtual machine is called what its machine is called: it
                // is renamed there, or its cable would point at nothing.
                if !net.lights.contains_key(&old) {
                    return Err(refused(reason!("network.rename_machine", node = old)));
                }
                if new != old {
                    free_name(&net, &new)?;
                    if room.join(format!("{new}.sock")).as_os_str().len() > PATH_MAX {
                        return Err(refused(reason!("network.path_too_long", path = new)));
                    }
                    net.rename_node(index, &new, &room);
                }
                Ok(data!({"node": old, "name": new}))
            }
            "link.add" => {
                let mut net = self.net()?;
                let a = port_named(&net, &text(action, "a"))?;
                // The second end's free port is looked for with the first one
                // taken into account, so a device linked to itself is refused
                // as such and not as a busy port.
                let b = port_named(&net, &text(action, "b"))?;
                net.link(a, b).map_err(refused)?;
                Ok(data!({"a": net.topology.end_text(a), "b": net.topology.end_text(b)}))
            }
            "link.remove" => {
                let mut net = self.net()?;
                let at = port_named(&net, &text(action, "at"))?;
                if !net.unlink(at) {
                    return Err(refused(reason!("network.no_such_cable")));
                }
                Ok(data!({"at": net.topology.end_text(at)}))
            }
            "port.configure" => {
                let mut net = self.net()?;
                let port = port_named(&net, &text(action, "port"))?;
                let mode =
                    Mode::from_value(&Value::Object(action.arguments.clone())).map_err(refused)?;
                let written = mode.to_value();
                net.configure_port(port, mode);
                Ok(data!({"port": net.topology.end_text(port), "mode": written}))
            }
            "node.place" => {
                let name = text(action, "node");
                let mut net = self.net()?;
                let node = net
                    .nodes
                    .iter()
                    .position(|n| n.name == name)
                    .ok_or_else(|| refused(reason!("network.no_such_node", node = name)))?;
                let at = port_named(&net, &text(action, "at"))?;
                net.place(node, at).map_err(refused)?;
                Ok(data!({"node": name, "at": net.topology.end_text(at)}))
            }
            "interface.configure" => {
                let vlan = vlan_of(action)?;
                let to = range(&text(action, "cidr"))?;
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let address = match action.argument("address").and_then(Value::as_str) {
                    Some(text) if !text.trim().is_empty() => text
                        .trim()
                        .parse::<Ipv4Addr>()
                        .map_err(|_| refused(reason!("network.bad_address", address = text)))?,
                    _ => gateway_of(to),
                };
                // A network that moves takes its machines, names and rules
                // along, as renumbering the main range does.
                let was = net
                    .side(router, vlan)
                    .map(|side| side.range)
                    .filter(|old| *old != to);
                let plan = match was {
                    Some(old) => {
                        let mut old_side = net.live.clone();
                        (old_side.network, old_side.prefix) = old;
                        let mine: Vec<Node> = net
                            .nodes
                            .iter()
                            .filter(|n| crate::switch::within(n.address, old.0, old.1))
                            .cloned()
                            .collect();
                        Some(renumber::plan(&old_side, &mine, to).map_err(refused)?)
                    }
                    None => None,
                };
                net.set_network(router, vlan, Some(Side { range: to, address }))
                    .map_err(refused)?;
                let mut moved: Vec<(String, Ipv4Addr, Ipv4Addr)> = Vec::new();
                if let Some(plan) = plan {
                    plan.apply_to_vlan(&mut net);
                    moved.extend(plan.nodes.iter().map(|m| (m.what.clone(), m.from, m.to)));
                }
                moved.extend(net.readdress());
                Ok(data!({
                    "router": net.topology.devices[router].name,
                    "vlan": vlan,
                    "cidr": format!("{}/{}", to.0, to.1),
                    "gateway": address.to_string(),
                    "moved": moved.iter().map(|(node, from, to)| json!({
                        "node": node, "from": from.to_string(), "to": to.to_string(),
                    })).collect::<Vec<_>>(),
                }))
            }
            "ipv6.set" => {
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let vlan = vlan_of(action)?;
                let on = action.argument("on").and_then(Value::as_bool) == Some(true);
                net.set_ipv6(router, vlan, on).map_err(refused)?;
                Ok(data!({"router": net.topology.devices[router].name, "vlan": vlan, "on": on}))
            }
            "nat.hide" => {
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let vlan = vlan_of(action)?;
                let on = action.argument("on").and_then(Value::as_bool) == Some(true);
                net.set_hidden(router, vlan, on).map_err(refused)?;
                Ok(data!({"router": net.topology.devices[router].name, "vlan": vlan, "on": on}))
            }
            "nat.add" => {
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let forward =
                    forward_of(&Value::Object(action.arguments.clone())).map_err(refused)?;
                let id = net.add_forward(router, forward).map_err(refused)?;
                Ok(data!({"id": id}))
            }
            "nat.remove" => {
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let id = text(action, "id");
                if net.remove_forward(router, &id) {
                    Ok(data!({"id": id}))
                } else {
                    Err(refused(reason!("network.no_such_forward", id = id)))
                }
            }
            "route.add" => {
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let to = parse_cidr(&text(action, "to"))
                    .filter(|(a, p)| {
                        u32::from(*a) & !(u32::MAX.checked_shr(u32::from(*p)).unwrap_or(0))
                            == u32::from(*a)
                    })
                    .ok_or_else(|| {
                        refused(reason!("network.bad_cidr", cidr = text(action, "to")))
                    })?;
                let via_text = text(action, "via");
                let via: Ipv4Addr = via_text
                    .parse()
                    .map_err(|_| refused(reason!("network.bad_address", address = via_text)))?;
                net.add_route(router, Route { to, via }).map_err(refused)?;
                Ok(data!({
                    "router": net.topology.devices[router].name,
                    "to": format!("{}/{}", to.0, to.1),
                    "via": via.to_string(),
                }))
            }
            "route.remove" => {
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let to = parse_cidr(&text(action, "to")).ok_or_else(|| {
                    refused(reason!("network.bad_cidr", cidr = text(action, "to")))
                })?;
                if !net.remove_route(router, to) {
                    return Err(refused(reason!(
                        "network.no_such_route",
                        to = text(action, "to")
                    )));
                }
                Ok(data!({
                    "router": net.topology.devices[router].name,
                    "to": format!("{}/{}", to.0, to.1),
                }))
            }
            "node.address" => {
                let name = text(action, "node");
                let wanted = text(action, "address");
                let mut net = self.net()?;
                let at = net
                    .nodes
                    .iter()
                    .position(|n| n.name == name)
                    .ok_or_else(|| refused(reason!("network.no_such_node", node = name)))?;
                let address: Ipv4Addr = wanted
                    .parse()
                    .map_err(|_| refused(reason!("network.bad_address", address = wanted)))?;
                // Inside the network the machine is in, and nobody else's.
                let range = net
                    .subnet_of(at)
                    .unwrap_or((net.live.network, net.live.prefix));
                if !crate::switch::host_in(address, range) {
                    return Err(refused(reason!(
                        "network.address_outside",
                        address = address,
                        cidr = format!("{}/{}", range.0, range.1),
                    )));
                }
                if net
                    .nodes
                    .iter()
                    .enumerate()
                    .any(|(i, n)| i != at && n.address == address)
                {
                    return Err(refused(reason!("network.address_taken", address = address)));
                }
                let from = net.nodes[at].address;
                net.move_node(&name, from, address);
                for value in net.live.dns.values_mut() {
                    if *value == from {
                        *value = address;
                    }
                }
                net.forget_conversations();
                Ok(data!({"node": name, "address": address.to_string(), "from": from.to_string()}))
            }
            "light.add" => {
                let name = text(action, "node");
                if self.net()?.node(&name).is_some() || self.net()?.topology.find(&name).is_some() {
                    return Err(refused(reason!("network.device_taken", device = name)));
                }
                let conf = match text(action, "kind").as_str() {
                    "server" => LightConf::server(),
                    _ => LightConf::pc(),
                };
                let at = action
                    .argument("at")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let mut done = self.attach_node(name.clone(), None, at)?;
                self.net()?.lights.insert(name.clone(), conf.clone());
                done.insert("light".into(), conf.to_value());
                Ok(data!({"node": name, "address": done["address"], "light": done["light"]}))
            }
            "light.configure" => {
                let name = text(action, "node");
                let mut net = self.net()?;
                let Some(mut conf) = net.lights.get(&name).cloned() else {
                    return Err(refused(reason!("network.no_such_node", node = name)));
                };
                let flag = |key: &str| action.argument(key).and_then(Value::as_bool);
                conf.dhcp = flag("dhcp").unwrap_or(conf.dhcp);
                conf.ping = flag("ping").unwrap_or(conf.ping);
                conf.web = flag("web").unwrap_or(conf.web);
                conf.echo = flag("echo").unwrap_or(conf.echo);
                for (key, slot) in [("gateway", &mut conf.gateway), ("dns", &mut conf.dns)] {
                    if let Some(text) = action.argument(key).and_then(Value::as_str) {
                        *slot = match text.trim() {
                            "" => None,
                            written => Some(written.parse().map_err(|_| {
                                refused(reason!("network.bad_address", address = written))
                            })?),
                        };
                    }
                }
                net.lights.insert(name.clone(), conf.clone());
                Ok(data!({"node": name, "light": conf.to_value()}))
            }
            "vlan.name" => {
                let vlan = vlan_of(action)?;
                let name = text(action, "name");
                if name.chars().count() > 24 || name.chars().any(char::is_control) {
                    return Err(refused(reason!("network.bad_vlan_name")));
                }
                let mut net = self.net()?;
                if name.is_empty() {
                    net.vlan_names.remove(&vlan);
                } else {
                    net.vlan_names.insert(vlan, name.clone());
                }
                Ok(data!({"vlan": vlan, "name": name}))
            }
            "light.run" => self.run_light(&text(action, "node"), &text(action, "line")),
            "light.start" => self.start_light(&text(action, "node"), &text(action, "line")),
            "light.result" => {
                let job = action.argument("job").and_then(Value::as_u64).unwrap_or(0);
                self.light_result(job)
            }
            "cable.configure" => {
                let mut net = self.net()?;
                let cable = text(action, "cable");
                let mut raw = Value::Object(action.arguments.clone());
                raw.as_object_mut().map(|o| o.remove("cable"));
                let traffic = traffic(&raw).map_err(refused)?;
                // A machine's cable by its name, or the cable at a device's port.
                if net.node(&cable).is_some() {
                    if traffic.is_perfect() {
                        net.node_traffic.remove(&cable);
                    } else {
                        net.node_traffic.insert(cable.clone(), traffic);
                    }
                } else {
                    let end = port_named(&net, &cable)?;
                    let other = net
                        .topology
                        .across(end)
                        .ok_or_else(|| refused(reason!("network.no_such_cable")))?;
                    if traffic.is_perfect() {
                        net.link_traffic.remove(&cable_key(end, other));
                    } else {
                        net.link_traffic.insert(cable_key(end, other), traffic);
                    }
                }
                let mut done = traffic_value(traffic)
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                done.insert("cable".into(), json!(cable));
                Ok(done)
            }
            "interface.remove" => {
                let vlan = vlan_of(action)?;
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let main = Some(router) == net.topology.router() && vlan == 1;
                if main || net.side(router, vlan).is_none() {
                    return Err(refused(reason!("network.no_such_interface", vlan = vlan)));
                }
                net.set_network(router, vlan, None).map_err(refused)?;
                Ok(data!({"router": net.topology.devices[router].name, "vlan": vlan}))
            }
            "network.flows" => {
                let net = self.net()?;
                Ok(data!({"flows": net.flows(), "lookups": net.lookups()}))
            }
            "firewall.add" => {
                let rule = rule(&Value::Object(action.arguments.clone())).map_err(refused)?;
                let mut net = self.net()?;
                for end in [&rule.source, &rule.destination] {
                    if let Some(group) = end.strip_prefix("group:")
                        && !net.live.groups.contains_key(group)
                    {
                        return Err(refused(reason!("network.no_such_group", group = group)));
                    }
                }
                let id = net.add_rule(rule);
                Ok(data!({"id": id}))
            }
            "capture.start" => self.start_capture(action),
            "capture.stop" => {
                let id = text(action, "id");
                let mut net = self.net()?;
                let capture = net
                    .captures
                    .iter_mut()
                    .find(|c| c.id == id)
                    .ok_or_else(|| refused(reason!("network.no_such_capture", id = id)))?;
                capture.stop();
                Ok(data!({"id": id}))
            }
            "capture.remove" => {
                let id = text(action, "id");
                let mut net = self.net()?;
                let at = net
                    .captures
                    .iter()
                    .position(|c| c.id == id)
                    .ok_or_else(|| refused(reason!("network.no_such_capture", id = id)))?;
                let mut capture = net.captures.remove(at);
                capture.stop();
                let _ = std::fs::remove_file(&capture.path);
                Ok(data!({"id": id}))
            }
            "capture.save" => self.save_capture(&text(action, "id")),
            "firewall.move" => {
                let id = text(action, "id");
                let index = action
                    .argument("index")
                    .and_then(Value::as_u64)
                    .map_or(usize::MAX, |i| usize::try_from(i).unwrap_or(usize::MAX));
                let mut net = self.net()?;
                if net.move_rule(&id, index) {
                    let index = net.live.rules.iter().position(|rule| rule.id == id);
                    Ok(data!({"id": id, "index": index}))
                } else {
                    Err(refused(reason!("network.no_such_rule", id = id)))
                }
            }
            "firewall.answers" => {
                let on = action.argument("on").and_then(Value::as_bool) == Some(true);
                self.net()?.set_answers(on);
                Ok(data!({"on": on}))
            }
            "firewall.group" => {
                let name = text(action, "name");
                if !label(&name) {
                    return Err(refused(reason!("network.bad_group", group = name)));
                }
                let mut net = self.net()?;
                let mut members: Vec<String> = Vec::new();
                for member in action
                    .argument("members")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let member = member.as_str().unwrap_or_default();
                    if net.node(member).is_none() {
                        return Err(refused(reason!("network.no_such_node", node = member)));
                    }
                    if !members.iter().any(|m| m == member) {
                        members.push(member.to_owned());
                    }
                }
                net.set_group(&name, members).map_err(refused)?;
                Ok(data!({"name": name}))
            }
            "firewall.service" => {
                let name = text(action, "name");
                if !label(&name) {
                    return Err(refused(reason!("network.bad_service", service = name)));
                }
                let mut arguments = action.arguments.clone();
                arguments.insert("action".into(), json!("allow"));
                let mut net = self.net()?;
                if action.argument("protocol").is_none() {
                    net.live.services.remove(&name);
                } else {
                    let rule = rule(&Value::Object(arguments)).map_err(refused)?;
                    let protocol = rule.protocol.ok_or_else(|| {
                        refused(reason!(
                            "network.bad_rule",
                            field = "protocol",
                            value = "any"
                        ))
                    })?;
                    net.live.services.insert(
                        name.clone(),
                        Service {
                            protocol,
                            ports: rule.ports,
                        },
                    );
                }
                Ok(data!({"name": name}))
            }
            "firewall.name" => {
                let rule = name_rule(&Value::Object(action.arguments.clone())).map_err(refused)?;
                let id = self.net()?.add_name(rule);
                Ok(data!({"id": id}))
            }
            "firewall.name_remove" => {
                let id = text(action, "id");
                if self.net()?.remove_name(&id) {
                    Ok(data!({"id": id}))
                } else {
                    Err(refused(reason!("network.no_such_rule", id = id)))
                }
            }
            "firewall.remove" => {
                let id = text(action, "id");
                if self.net()?.remove_rule(&id) {
                    Ok(data!({"id": id}))
                } else {
                    Err(refused(reason!("network.no_such_rule", id = id)))
                }
            }
            "firewall.default" => {
                let policy = text(action, "policy");
                let allow = match policy.as_str() {
                    "allow" => true,
                    "deny" => false,
                    _ => {
                        return Err(refused(reason!(
                            "network.bad_rule",
                            field = "default",
                            value = policy
                        )));
                    }
                };
                self.net()?.set_default(allow);
                Ok(data!({"policy": policy}))
            }
            "network.renumber_plan" => {
                let to = range(&text(action, "cidr"))?;
                let net = self.net()?;
                let plan = main_plan(&net, to)?;
                Ok(plan.describe(&self.config.settings))
            }
            "router.internet" => {
                let on = action.argument("on").and_then(Value::as_bool) == Some(true);
                self.net()?.set_internet(on);
                Ok(data!({"on": on}))
            }
            "dns.add" => {
                let name = text(action, "name")
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                let address = text(action, "address");
                let parsed: Ipv4Addr = address
                    .parse()
                    .map_err(|_| refused(reason!("network.bad_address", address = address)))?;
                if name.is_empty() {
                    return Err(refused(reason!("network.bad_name", name = name)));
                }
                let mut net = self.net()?;
                net.live.dns.insert(name.clone(), parsed);
                // Who put it there: the Server says; a person does not.
                match text(action, "from") {
                    from if from.is_empty() => net.published.remove(&name),
                    from => net.published.insert(name.clone(), from),
                };
                Ok(data!({"name": name, "address": address}))
            }
            "dns.remove" => {
                let name = text(action, "name").to_ascii_lowercase();
                let mut net = self.net()?;
                net.published.remove(&name);
                match net.live.dns.remove(&name) {
                    Some(_) => Ok(data!({"name": name})),
                    None => Err(refused(reason!("network.bad_name", name = name))),
                }
            }
            "dns.record" => {
                let record =
                    record_of(&Value::Object(action.arguments.clone())).map_err(refused)?;
                let mut net = self.net()?;
                if !net.live.records.contains(&record) {
                    net.live.records.push(record.clone());
                }
                let value = record_value(&record);
                Ok(data!({"name": value["name"], "type": value["type"], "value": value["value"]}))
            }
            "dns.record_remove" => {
                let name = text(action, "name")
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                let kind = text(action, "type").to_ascii_uppercase();
                let value = text(action, "value");
                let mut net = self.net()?;
                let before = net.live.records.len();
                net.live.records.retain(|record| {
                    let raw = record_value(record);
                    !(raw["name"] == name.as_str()
                        && raw["type"] == kind.as_str()
                        && raw["value"] == value.as_str())
                });
                if net.live.records.len() == before {
                    return Err(refused(reason!("network.bad_name", name = name)));
                }
                Ok(data!({"name": name}))
            }
            "dns.domain" => {
                let domain = text(action, "domain")
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if !dns_name(&domain) {
                    return Err(refused(reason!("network.bad_dns_name", name = domain)));
                }
                self.net()?.live.domain = domain.clone();
                Ok(data!({"domain": domain}))
            }
            "dhcp.configure" => {
                let mut net = self.net()?;
                let router = router_named(&net, action)?;
                let vlan = vlan_of(action)?;
                let mut dhcp = net.dhcp_of(router, vlan);
                let number = |key: &str| {
                    action
                        .argument(key)
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                };
                if let Some(on) = action.argument("on").and_then(Value::as_bool) {
                    dhcp.on = on;
                }
                if let Some(first) = number("first") {
                    dhcp.first = first;
                }
                // 0 is to the end of the range.
                if let Some(last) = number("last") {
                    dhcp.last = (last > 0).then_some(last);
                }
                if let Some(lease) = number("lease") {
                    dhcp.lease = lease;
                }
                if action.argument("dns").is_some() {
                    let dns = text(action, "dns");
                    dhcp.dns =
                        if dns.is_empty() {
                            None
                        } else {
                            Some(dns.parse().map_err(|_| {
                                refused(reason!("network.bad_address", address = dns))
                            })?)
                        };
                }
                net.set_dhcp(router, vlan, dhcp).map_err(refused)?;
                Ok(data!({
                    "router": net.topology.devices[router].name,
                    "vlan": vlan,
                    "on": dhcp.on,
                    "first": dhcp.first,
                    "last": dhcp.last,
                    "lease": dhcp.lease,
                    "dns": dhcp.dns.map(|a| a.to_string()),
                }))
            }
            "node.reserve" => {
                let node = text(action, "node");
                let on = action.argument("on").and_then(Value::as_bool) == Some(true);
                let mut net = self.net()?;
                if net.node(&node).is_none() {
                    return Err(refused(reason!("network.no_such_node", node = node)));
                }
                if on {
                    net.reserved.insert(node.clone());
                } else {
                    net.reserved.remove(&node);
                }
                Ok(data!({"node": node, "on": on}))
            }
            "dns.resolve" => {
                let name = text(action, "name");
                let address = self.net()?.resolve(&name).map(|a| a.to_string());
                Ok(data!({"name": name, "address": address}))
            }
            "traffic.configure" => {
                let mut net = self.net()?;
                let mut raw = traffic_value(net.live.traffic);
                for (key, value) in &action.arguments {
                    raw[key] = value.clone();
                }
                net.live.traffic = traffic(&raw).map_err(refused)?;
                let now = traffic_value(net.live.traffic);
                drop(net);
                self.emit(
                    "traffic.configured",
                    now.as_object().cloned().unwrap_or_default(),
                );
                Ok(now.as_object().cloned().unwrap_or_default())
            }
            "traffic.observe" => {
                let net = self.net()?;
                Ok(data!({"counters": counters(&net), "traffic": traffic_value(net.live.traffic)}))
            }
            other => Err(beatbox_engine::runtime::provider::unsupported(other)),
        }
    }
}

fn refused(reason: Reason) -> ProviderError {
    ProviderError::new(reason)
}

fn text(action: &Action, key: &str) -> String {
    action
        .argument(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Lowercase letters, digits, `-` and `_`, starting with a letter: it is a
/// DNS label and a file name.
fn node_name(name: &str) -> bool {
    name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && name != "gateway"
        && name != "any"
        && name != "internet"
}

#[cfg(unix)]
fn make_room(room: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    // 0700: any process that can reach the switch's socket can put frames on
    // this network, and only this user's processes should.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(room)
}

#[cfg(not(unix))]
fn make_room(room: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(room)
}

/// A range a network may have: `10.20.0.0/24`, a /8 to a /29, written at
/// its first address.
fn range(cidr: &str) -> Result<(Ipv4Addr, u8), ProviderError> {
    parse_cidr(cidr)
        .filter(|(address, prefix)| {
            (8..=29).contains(prefix)
                && u32::from(*address) & !(u32::MAX >> prefix) == u32::from(*address)
        })
        .ok_or_else(|| refused(reason!("network.bad_cidr", cidr = cidr)))
}

fn vlan_of(action: &Action) -> Result<u16, ProviderError> {
    action
        .argument("vlan")
        .and_then(Value::as_u64)
        .and_then(|v| u16::try_from(v).ok())
        .ok_or_else(|| {
            let said = action
                .argument("vlan")
                .map_or_else(String::new, Value::to_string);
            refused(reason!("network.bad_vlan", vlan = said))
        })
}

/// Renumbering the main range: only the machines in it move, and it may
/// not take addresses another of the router's networks has.
fn main_plan(net: &Net, to: (Ipv4Addr, u8)) -> Result<renumber::Plan, ProviderError> {
    let others: Vec<(Ipv4Addr, u8)> = net
        .router_devices()
        .into_iter()
        .flat_map(|device| net.sides(device))
        .filter(|(vlan, _)| *vlan != 1)
        .map(|(_, side)| side.range)
        .collect();
    if let Some(other) = others.iter().find(|other| overlap(to, **other)) {
        return Err(refused(reason!(
            "network.overlap",
            cidr = format!("{}/{}", to.0, to.1),
            other = format!("{}/{}", other.0, other.1)
        )));
    }
    let mine: Vec<Node> = net
        .nodes
        .iter()
        .filter(|node| net.live.contains(node.address))
        .cloned()
        .collect();
    renumber::plan(&net.live, &mine, to).map_err(refused)
}

fn cidr_text(baseline: &Baseline) -> String {
    format!("{}/{}", baseline.network, baseline.prefix)
}

/// What a host is handed when it is cabled here -- the whole contract
/// between the two. `switch`, `port` and `mac` are the cable (frames, as
/// datagrams, from `port` to `switch`); the rest is what the guest will be
/// told by DHCP, said up front so nobody has to boot it to find out.
fn attachment(net: &Net, node: &Node, switch: &std::path::Path) -> Data {
    let served = net
        .nodes
        .iter()
        .position(|n| n.name == node.name)
        .and_then(|at| net.serving(at));
    let range = served.map_or((net.live.network, net.live.prefix), |(_, _, side)| {
        side.range
    });
    let gateway = served.map_or(net.live.gateway(), |(_, _, side)| side.address);
    let dns = served
        .and_then(|(router, vlan, _)| net.dhcp_of(router, vlan).dns)
        .unwrap_or(gateway);
    data!({
        "node": node.name,
        "address": node.address.to_string(),
        "prefix": range.1,
        "gateway": gateway.to_string(),
        "dns": [dns.to_string()],
        "mac": mac_text(node.mac),
        "switch": switch.to_string_lossy(),
        "port": node.port.to_string_lossy(),
    })
}

fn counters(net: &Net) -> Value {
    let c = net.counters;
    json!({
        "received": c.received,
        "delivered": c.delivered,
        "blocked": c.blocked,
        "lost": c.lost,
        "refused": c.refused,
        "unroutable": c.unroutable,
    })
}

fn facts(net: &Net, room: &std::path::Path) -> Data {
    data!({
        "cidr": cidr_text(&net.live),
        "gateway": net.live.gateway().to_string(),
        "nodes": net.nodes.iter().enumerate().map(|(i, node)| json!({
            "node": node.name,
            "address": node.address.to_string(),
            "addresses6": net.addresses6_of(i).iter().map(ToString::to_string).collect::<Vec<_>>(),
            "mac": mac_text(node.mac),
            "at": net.topology.end_text(node.at),
            "light": net.lights.get(&node.name).map(LightConf::to_value),
            "reserved": net.reserved.contains(&node.name),
            "lease_ms": net.leases.get(&node.name),
        })).collect::<Vec<_>>(),
        "topology": net.topology.to_value(),
        "vlan_names": net.vlan_names.iter().map(|(v, n)| (v.to_string(), json!(n))).collect::<serde_json::Map<_, _>>(),
        "cable_traffic": net.node_traffic.iter().map(|(node, traffic)| {
            let mut value = traffic_value(*traffic);
            value["node"] = json!(node);
            value
        }).chain(net.link_traffic.iter().map(|((a, b), traffic)| {
            let mut value = traffic_value(*traffic);
            value["a"] = json!(net.topology.end_text(*a));
            value["b"] = json!(net.topology.end_text(*b));
            value
        })).collect::<Vec<_>>(),
        "cable_frames": net.node_frames.iter().map(|(node, (sent, received))| json!({
            "node": node, "sent": sent, "received": received,
        })).chain(net.link_frames.iter().map(|((a, b), frames)| json!({
            "a": net.topology.end_text(*a), "b": net.topology.end_text(*b), "frames": frames,
        }))).collect::<Vec<_>>(),
        "captures": net.captures.iter().map(|c| json!({
            "id": c.id,
            "node": match &c.tap { Tap::Node(name) => Some(name.clone()), Tap::Link(_) => None },
            "a": match &c.tap { Tap::Link((a, _)) => Some(net.topology.end_text(*a)), Tap::Node(_) => None },
            "b": match &c.tap { Tap::Link((_, b)) => Some(net.topology.end_text(*b)), Tap::Node(_) => None },
            "keep": c.keep.name(),
            "talk": c.talk.is_some(),
            "bytes": c.bytes,
            "frames": c.frames,
            "started_ms": c.started_ms,
            "ended_ms": c.ended_ms,
            "full": c.full,
            "live": c.live(),
        })).collect::<Vec<_>>(),
        "interfaces": net.topology.router().map(|main| net.sides(main)).unwrap_or_default().iter()
            .filter(|(vlan, _)| *vlan != 1)
            .map(|(vlan, side)| json!({
                "vlan": vlan,
                "cidr": format!("{}/{}", side.range.0, side.range.1),
                "gateway": side.address.to_string(),
            })).collect::<Vec<_>>(),
        "routers": net.router_devices().into_iter().map(|device| json!({
            "name": net.topology.devices[device].name,
            "main": Some(device) == net.topology.router(),
            "networks": net.sides(device).iter().map(|(vlan, side)| json!({
                "vlan": vlan,
                "cidr": format!("{}/{}", side.range.0, side.range.1),
                "address": side.address.to_string(),
                "prefix6": format!("{}/64", prefix6(side.range)),
                "address6": gateway6(side.range).to_string(),
                "ipv6": net.speaks6(device, *vlan),
                "dhcp": dhcp_value(net.dhcp_of(device, *vlan)),
                "pool": pool_value(net, side.range),
            })).collect::<Vec<_>>(),
            "ipv6": net.routers.get(&net.topology.devices[device].name)
                .map(|conf| conf.ipv6.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default(),
            "routes": net.routers.get(&net.topology.devices[device].name)
                .map(|conf| conf.routes.iter().map(|r| json!({
                    "to": format!("{}/{}", r.to.0, r.to.1),
                    "via": r.via.to_string(),
                })).collect::<Vec<_>>())
                .unwrap_or_default(),
            "hidden": net.routers.get(&net.topology.devices[device].name)
                .map(|conf| conf.hidden.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default(),
            "forwards": net.routers.get(&net.topology.devices[device].name)
                .map(|conf| conf.forwards.iter().map(|f| json!({
                    "id": f.id,
                    "protocol": frame::protocol_name(f.protocol),
                    "port": f.port,
                    "to": f.to,
                    "to_port": f.to_port,
                    "uses": f.uses,
                    "quiet_ms": f.last.map(|at| at.elapsed().as_millis() as u64),
                })).collect::<Vec<_>>())
                .unwrap_or_default(),
        })).collect::<Vec<_>>(),
        "firewall": {
            "default": if net.live.default_allow { "allow" } else { "deny" },
            "rules": net.live.rules.iter().map(rule_value).collect::<Vec<_>>(),
            "names": net.live.names.iter().map(name_value).collect::<Vec<_>>(),
            "answers": net.live.answers,
            "groups": net.live.groups,
            "services": net.live.services.iter().map(|(name, service)| (name.clone(), json!({
                "protocol": frame::protocol_name(service.protocol),
                "port": service.ports.map(|p| p.0),
                "port_last": service.ports.map(|p| p.1),
            }))).collect::<serde_json::Map<_, _>>(),
        },
        "dns": net.live.dns.iter().map(|(k, v)| (k.clone(), json!(v.to_string()))).collect::<serde_json::Map<_, _>>(),
        "dns_from": net.published,
        "records": net.live.records.iter().map(record_value).collect::<Vec<_>>(),
        "domain": net.live.domain,
        "traffic": traffic_value(net.live.traffic),
        "internet": net.live.internet,
        "counters": counters(net),
        "workspace": room.to_string_lossy(),
    })
}

/// What makes the network what it is, as `net.json` and an exported file
/// keep it: no captures, no counts.
fn kept(net: &Net, room: &std::path::Path) -> Data {
    let mut state = facts(net, room);
    state.retain(|key, _| {
        matches!(
            key.as_str(),
            "cidr"
                | "nodes"
                | "firewall"
                | "dns"
                | "records"
                | "domain"
                | "traffic"
                | "internet"
                | "topology"
                | "routers"
                | "cable_traffic"
                | "vlan_names"
        )
    });
    // What the Server published goes with the Server.
    if let Some(Value::Object(dns)) = state.get_mut("dns") {
        dns.retain(|name, _| !net.published.contains_key(name));
    }
    state
}

/// The first and last address handed out in `range`.
fn pool_value(net: &Net, range: (Ipv4Addr, u8)) -> Value {
    let (first, last) = net.pool(range);
    json!([
        Ipv4Addr::from(first).to_string(),
        Ipv4Addr::from(last).to_string()
    ])
}

/// How a router hands out addresses, as `dhcp_value` wrote it.
fn dhcp_of(raw: &Value) -> Result<crate::switch::Dhcp, Reason> {
    let base = crate::switch::Dhcp::default();
    let number = |key: &str| {
        raw.get(key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let dns = match raw.get("dns").and_then(Value::as_str) {
        None => None,
        Some(text) => Some(
            text.parse()
                .map_err(|_| reason!("network.bad_address", address = text))?,
        ),
    };
    Ok(crate::switch::Dhcp {
        on: raw.get("on").and_then(Value::as_bool).unwrap_or(base.on),
        first: number("first").unwrap_or(base.first),
        last: number("last"),
        lease: number("lease").unwrap_or(base.lease),
        dns,
    })
}

/// How a router hands out addresses, as `dhcp.configure` and `net.json` say it.
fn dhcp_value(dhcp: crate::switch::Dhcp) -> Value {
    json!({
        "on": dhcp.on,
        "first": dhcp.first,
        "last": dhcp.last,
        "lease": dhcp.lease,
        "dns": dhcp.dns.map(|a| a.to_string()),
    })
}

/// The network as a file to give someone (`network.import` reads it).
fn exported(net: &Net, room: &std::path::Path) -> Data {
    let mut state = kept(net, room);
    state.insert("format".into(), json!(EXPORT_FORMAT));
    state.insert("version".into(), json!(EXPORT_VERSION));
    state
}

/// What an exported network says it is.
const EXPORT_FORMAT: &str = "beatbox-network";
const EXPORT_VERSION: u64 = 1;

/// A save point's id as `point.save` makes it: `p` and the time.
fn point_id(id: &str) -> bool {
    id.strip_prefix('p')
        .is_some_and(|ms| !ms.is_empty() && ms.chars().all(|c| c.is_ascii_digit()))
}

/// Letters, digits, `-` and `_`; anything else a `-`. For file names.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `stem.ext` in `folder`, numbered when taken: nothing already there is
/// written over.
fn free_file(folder: &std::path::Path, stem: &str, ext: &str) -> PathBuf {
    let mut to = folder.join(format!("{stem}.{ext}"));
    for n in 2.. {
        if !to.exists() {
            break;
        }
        to = folder.join(format!("{stem}-{n}.{ext}"));
    }
    to
}

/// Where a machine goes when nobody says: the first switch, as in a real
/// network; the router when there is none.
fn first_place(net: &Net) -> End {
    let device = net
        .topology
        .devices
        .iter()
        .position(|d| d.kind == topology::Kind::Switch)
        .or(net.topology.router())
        .unwrap_or(0);
    End {
        device,
        port: net.free_port(device),
    }
}

/// The router an action names (`router`), or the main one.
/// A forward as `nat.add` and `net.json` say it: TCP or UDP, a port on the
/// router, a machine and a port on it.
fn forward_of(raw: &Value) -> Result<Forward, Reason> {
    let port = |key: &str| {
        raw.get(key)
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p > 0)
            .ok_or_else(|| reason!("network.bad_forward", field = key))
    };
    let protocol = match raw
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "tcp" => frame::TCP,
        "udp" => frame::UDP,
        _ => return Err(reason!("network.bad_forward", field = "protocol")),
    };
    let to = raw
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !node_name(&to) {
        return Err(reason!("network.bad_forward", field = "to"));
    }
    Ok(Forward {
        id: String::new(),
        protocol,
        port: port("port")?,
        to,
        to_port: port("to_port")?,
        uses: 0,
        last: None,
    })
}

fn router_named(net: &Net, action: &Action) -> Result<usize, ProviderError> {
    let name = text(action, "router");
    let found = if name.is_empty() {
        net.topology.router()
    } else {
        net.topology
            .find(&name)
            .filter(|d| net.topology.devices[*d].kind == topology::Kind::Router)
    };
    found.ok_or_else(|| refused(reason!("network.no_such_device")))
}

/// Whether `name` can be taken by a new device or node: well formed, and
/// nothing on the network is called it already.
fn free_name(net: &Net, name: &str) -> Result<(), ProviderError> {
    if !node_name(name) {
        return Err(refused(reason!("network.bad_device", device = name)));
    }
    if net.topology.find(name).is_some() || net.node(name).is_some() {
        return Err(refused(reason!("network.device_taken", device = name)));
    }
    Ok(())
}

/// `sw1:3`, read; `None` when there is no such device or no port.
fn end_at(net: &Net, text: &str) -> Option<End> {
    let (device, port) = net.topology.parse_end(text)?;
    Some(End {
        device,
        port: port?,
    })
}

/// `sw1:3`, or `sw1` for its first free port.
fn port_named(net: &Net, text: &str) -> Result<End, ProviderError> {
    let (device, port) = net
        .topology
        .parse_end(text)
        .ok_or_else(|| refused(reason!("network.no_such_device")))?;
    Ok(End {
        device,
        port: port.unwrap_or_else(|| net.free_port(device)),
    })
}

fn rule_value(rule: &Rule) -> Value {
    json!({
        "id": rule.id,
        "action": rule.verdict.name(),
        "state": if rule.only_new { "new" } else { "any" },
        "source": rule.source,
        "destination": rule.destination,
        "protocol": rule.protocol.map_or_else(|| "any".to_owned(), frame::protocol_name),
        "port": rule.ports.map(|p| p.0),
        "port_last": rule.ports.map(|p| p.1),
        "icmp_type": rule.icmp_type,
        "ethertype": rule.ethertype,
        "service": rule.service,
        "family": rule.family,
        "limit": rule.limit.map(|limit| json!({
            "count": limit.count,
            "per": match limit.per_ms { 1_000 => "second", 3_600_000 => "hour", _ => "minute" },
            "each": limit.each,
            "over": limit.over.name(),
        })),
    })
}

fn traffic_value(traffic: Traffic) -> Value {
    json!({
        "latency_ms": traffic.latency_ms,
        "jitter_ms": traffic.jitter_ms,
        "loss_percent": traffic.loss_percent,
        "bandwidth_kbps": traffic.bandwidth_kbps,
    })
}

/// A network's settings, read. Everything is checked here, at `create`, so
/// a bad rule is refused before anything runs rather than ignored after.
pub fn baseline(settings: &Data) -> Result<Baseline, Reason> {
    let cidr = settings
        .get("cidr")
        .and_then(Value::as_str)
        .unwrap_or("10.10.10.0/24");
    let (network, prefix) = parse_cidr(cidr)
        .filter(|(address, prefix)| {
            (8..=29).contains(prefix)
                && u32::from(*address) & !(u32::MAX >> prefix) == u32::from(*address)
        })
        .ok_or_else(|| reason!("network.bad_cidr", cidr = cidr))?;
    let firewall = settings.get("firewall").cloned().unwrap_or(Value::Null);
    let default_allow = match firewall.get("default").and_then(Value::as_str) {
        None | Some("allow") => true,
        Some("deny") => false,
        Some(other) => {
            return Err(reason!(
                "network.bad_rule",
                field = "default",
                value = other
            ));
        }
    };
    let mut rules = Vec::new();
    for (index, raw) in firewall
        .get("rules")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let mut rule = rule(raw)?;
        rule.id = raw
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("r{}", index + 1), ToOwned::to_owned);
        rules.push(rule);
    }
    let answers = firewall
        .get("answers")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut groups = BTreeMap::new();
    for (name, members) in firewall
        .get("groups")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let members: Option<Vec<String>> = members.as_array().map(|m| {
            m.iter()
                .filter_map(|m| m.as_str().map(ToOwned::to_owned))
                .collect()
        });
        match members {
            // Empty when its machines were all unplugged since.
            Some(members) if label(name) => {
                groups.insert(name.clone(), members);
            }
            _ => return Err(reason!("network.bad_group", group = name)),
        }
    }
    let mut services = BTreeMap::new();
    for (name, raw) in firewall
        .get("services")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let mut raw = raw.clone();
        raw["action"] = json!("allow");
        let rule = rule(&raw)?;
        match rule.protocol {
            Some(protocol) if label(name) => {
                services.insert(
                    name.clone(),
                    Service {
                        protocol,
                        ports: rule.ports,
                    },
                );
            }
            _ => return Err(reason!("network.bad_service", service = name)),
        }
    }
    let mut dns = BTreeMap::new();
    for (name, address) in settings
        .get("dns")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let text = address.as_str().unwrap_or_default();
        let address = text
            .parse()
            .map_err(|_| reason!("network.bad_address", address = text))?;
        dns.insert(name.trim_end_matches('.').to_ascii_lowercase(), address);
    }
    let records = settings
        .get("records")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(record_of)
        .collect::<Result<Vec<_>, _>>()?;
    let domain = match settings.get("domain").and_then(Value::as_str) {
        None => frame::DOMAIN.to_owned(),
        Some(domain) if dns_name(domain) => domain.trim_end_matches('.').to_ascii_lowercase(),
        Some(domain) => return Err(reason!("network.bad_dns_name", name = domain)),
    };
    let traffic = match settings.get("traffic") {
        Some(raw) => traffic(raw)?,
        None => Traffic::default(),
    };
    let internet = match settings.get("internet") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(on)) => *on,
        Some(other) => {
            return Err(reason!(
                "network.bad_rule",
                field = "internet",
                value = other
            ));
        }
    };
    let mut names = Vec::new();
    for raw in firewall
        .get("names")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut rule = name_rule(raw)?;
        rule.id = raw
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("n{}", names.len() + 1), ToOwned::to_owned);
        names.push(rule);
    }
    Ok(Baseline {
        network,
        prefix,
        default_allow,
        rules,
        answers,
        groups,
        services,
        dns,
        records,
        domain,
        traffic,
        internet,
        names,
    })
}

/// Whether `name` may be a DNS name: labels of letters, digits, `-` and
/// `_`, none empty or past 63, 253 in all.
fn dns_name(name: &str) -> bool {
    let name = name.trim_end_matches('.');
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// A record as `dns.record` and `net.json` say it: a name, its `type`
/// (`AAAA`, `CNAME`, `MX`, `TXT`), its `value`, and an MX's `priority`.
fn record_of(raw: &Value) -> Result<(String, frame::Record), Reason> {
    let text = |key: &str| raw.get(key).and_then(Value::as_str).unwrap_or_default();
    let name = text("name").trim_end_matches('.').to_ascii_lowercase();
    if !dns_name(&name) {
        return Err(reason!("network.bad_dns_name", name = text("name")));
    }
    let value = text("value").trim();
    let kind = text("type").to_ascii_uppercase();
    let target = || {
        let to = value.trim_end_matches('.').to_ascii_lowercase();
        if dns_name(&to) && to != name {
            Ok(to)
        } else {
            Err(reason!("network.bad_dns_name", name = value))
        }
    };
    let record = match kind.as_str() {
        "AAAA" => frame::Record::Aaaa(
            value
                .parse()
                .map_err(|_| reason!("network.bad_address", address = value))?,
        ),
        "CNAME" => frame::Record::Cname(target()?),
        "MX" => {
            let priority = raw
                .get("priority")
                .map_or(Some(10), |p| p.as_u64().and_then(|p| u16::try_from(p).ok()));
            let priority = priority.ok_or_else(|| {
                reason!(
                    "network.bad_rule",
                    field = "priority",
                    value = raw["priority"].to_string()
                )
            })?;
            frame::Record::Mx(priority, target()?)
        }
        "TXT" if !value.is_empty() && value.len() <= 1024 => frame::Record::Txt(value.to_owned()),
        "TXT" => return Err(reason!("network.bad_rule", field = "value", value = value)),
        _ => return Err(reason!("network.bad_rule", field = "type", value = kind)),
    };
    Ok((name, record))
}

/// A record as `record_of` reads it.
fn record_value((name, record): &(String, frame::Record)) -> Value {
    let (kind, value, priority) = match record {
        frame::Record::A(a) => ("A", a.to_string(), None),
        frame::Record::Aaaa(a) => ("AAAA", a.to_string(), None),
        frame::Record::Cname(to) => ("CNAME", to.clone(), None),
        frame::Record::Mx(p, to) => ("MX", to.clone(), Some(*p)),
        frame::Record::Txt(text) => ("TXT", text.clone(), None),
    };
    json!({"name": name, "type": kind, "value": value, "priority": priority})
}

/// A rule by name as `firewall.name` and `net.json` say it: `hide`, `point`
/// (with `to`, a machine or an address) or `cut`; a name, `*.` in front for
/// everything under it; who asks, as a rule's source.
fn name_rule(raw: &Value) -> Result<NameRule, Reason> {
    let bad = |key: &str, value: &dyn std::fmt::Display| {
        reason!("network.bad_rule", field = key, value = value.to_string())
    };
    let text = |key: &str| {
        raw.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    };
    let name = text("name").trim_end_matches('.').to_owned();
    let base = name.strip_prefix("*.").unwrap_or(&name);
    let fine = !base.is_empty()
        && base.len() <= 253
        && base.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if !fine {
        return Err(bad("name", &name));
    }
    let action = match text("action").as_str() {
        "hide" => NameAction::Hide,
        "cut" => NameAction::Cut,
        "point" => {
            let to = text("to");
            if !(node_name(&to) || to.parse::<std::net::IpAddr>().is_ok()) {
                return Err(bad("to", &to));
            }
            NameAction::Point(to)
        }
        other => return Err(bad("action", &other)),
    };
    // Who asks is read as a rule's source is.
    let source = rule(
        &json!({"action": "deny", "source": raw.get("source").cloned().unwrap_or(json!("any"))}),
    )?
    .source;
    Ok(NameRule {
        id: String::new(),
        action,
        name,
        source,
        uses: 0,
    })
}

fn name_value(rule: &NameRule) -> Value {
    json!({
        "id": rule.id,
        "action": rule.action.word(),
        "name": rule.name,
        "source": rule.source,
        "to": match &rule.action { NameAction::Point(to) => Some(to.clone()), _ => None },
        "uses": rule.uses,
    })
}

fn rule(raw: &Value) -> Result<Rule, Reason> {
    let field = |key: &str| {
        raw.get(key)
            .and_then(Value::as_str)
            .unwrap_or("any")
            .trim()
            .to_ascii_lowercase()
    };
    let bad = |key: &str, value: &dyn std::fmt::Display| {
        reason!("network.bad_rule", field = key, value = value.to_string())
    };
    let verdict = match field("action").as_str() {
        "allow" => Verdict::Allow,
        "deny" => Verdict::Deny,
        "reject" => Verdict::Reject,
        other => return Err(bad("action", &other)),
    };
    let only_new = match field("state").as_str() {
        "any" | "" => false,
        "new" => true,
        other => return Err(bad("state", &other)),
    };
    let mut protocol = match field("protocol").as_str() {
        "any" | "" => None,
        other => Some(frame::protocol_number(other).ok_or_else(|| bad("protocol", &other))?),
    };
    let number = |key: &str, least: u64, most: u64| -> Result<Option<u64>, Reason> {
        match raw.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .filter(|n| (least..=most).contains(n))
                .map(Some)
                .ok_or_else(|| bad(key, value)),
        }
    };
    let port =
        |key: &str| -> Result<Option<u16>, Reason> { Ok(number(key, 1, 65535)?.map(|p| p as u16)) };
    let ports = match (port("port")?, port("port_last")?) {
        (None, None) => None,
        (Some(first), last) if last.is_none_or(|last| last >= first) => {
            Some((first, last.unwrap_or(first)))
        }
        (_, last) => return Err(bad("port_last", &last.unwrap_or_default())),
    };
    let icmp_type = number("icmp_type", 0, 255)?.map(|t| t as u8);
    if icmp_type.is_some() {
        if protocol.is_some_and(|p| p != frame::ICMP) {
            return Err(bad("icmp_type", &field("protocol")));
        }
        protocol = Some(frame::ICMP);
    }
    // Not IP: IPv4 and IPv6 are what the other rules judge, and ARP is
    // never judged.
    let ethertype = number("ethertype", 0x0600, 0xffff)?
        .map(|e| e as u16)
        .filter(|e| ![frame::ETHER_IPV4, frame::ETHER_IPV6, frame::ETHER_ARP].contains(e));
    let family = number("family", 4, 6)?.map(|f| f as u8);
    if family.is_some_and(|f| f == 5) {
        return Err(bad("family", &5));
    }
    if raw.get("ethertype").is_some_and(|e| !e.is_null())
        && (ethertype.is_none() || protocol.is_some() || ports.is_some())
    {
        return Err(bad("ethertype", &raw["ethertype"]));
    }
    let endpoint = |key: &str| -> Result<String, Reason> {
        let raw_spec = raw.get(key).and_then(Value::as_str).unwrap_or("any").trim();
        // A group keeps the name as it was written.
        if let Some(group) = raw_spec.strip_prefix("group:") {
            return if label(group) {
                Ok(raw_spec.to_owned())
            } else {
                Err(bad(key, &raw_spec))
            };
        }
        let spec = field(key);
        let vlan = spec
            .strip_prefix("vlan:")
            .and_then(|v| v.parse::<u16>().ok())
            .is_some_and(|v| (1..=4094).contains(&v));
        let fine = matches!(spec.as_str(), "any" | "*" | "gateway" | "internet")
            || vlan
            || node_name(&spec)
            || parse_cidr(&spec).is_some()
            || parse_cidr6(&spec).is_some();
        if fine { Ok(spec) } else { Err(bad(key, &spec)) }
    };
    let limit = match raw.get("limit") {
        None | Some(Value::Null) => None,
        Some(asked) => {
            let count = asked
                .get("count")
                .and_then(Value::as_u64)
                .filter(|n| (1..=100_000).contains(n))
                .ok_or_else(|| bad("limit", &asked))? as u32;
            let per_ms = match asked.get("per").and_then(Value::as_str).unwrap_or("minute") {
                "second" => 1_000,
                "minute" => 60_000,
                "hour" => 3_600_000,
                other => return Err(bad("limit", &other)),
            };
            let over = match asked.get("over").and_then(Value::as_str).unwrap_or("deny") {
                "deny" => Verdict::Deny,
                "reject" => Verdict::Reject,
                other => return Err(bad("limit", &other)),
            };
            // A limit says how much an allowing rule lets through.
            if verdict != Verdict::Allow {
                return Err(bad("limit", &field("action")));
            }
            Some(Limit {
                count,
                per_ms,
                each: asked.get("each").and_then(Value::as_bool).unwrap_or(true),
                over,
            })
        }
    };
    let service = raw
        .get("service")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    if service.as_deref().is_some_and(|s| !label(s)) {
        return Err(bad("service", &service.unwrap_or_default()));
    }
    Ok(Rule {
        id: String::new(),
        verdict,
        only_new,
        source: endpoint("source")?,
        destination: endpoint("destination")?,
        protocol,
        ports,
        icmp_type,
        ethertype,
        service,
        limit,
        family,
    })
}

/// A name people give a group or a service: up to 24 characters, printable.
fn label(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 24
        && !name.chars().any(|c| c.is_control() || c == ':')
}

fn traffic(raw: &Value) -> Result<Traffic, Reason> {
    let number = |key: &str, most: f64| -> Result<f64, Reason> {
        match raw.get(key) {
            None | Some(Value::Null) => Ok(0.0),
            Some(value) => value
                .as_f64()
                .filter(|n| (0.0..=most).contains(n))
                .ok_or_else(|| reason!("network.bad_traffic", field = key, value = value)),
        }
    };
    Ok(Traffic {
        latency_ms: number("latency_ms", 60_000.0)?,
        jitter_ms: number("jitter_ms", 60_000.0)?,
        loss_percent: number("loss_percent", 100.0)?,
        bandwidth_kbps: number("bandwidth_kbps", 10_000_000.0)?,
    })
}

/// The person's Downloads folder: what `xdg-user-dir` says, else `~/Downloads`
/// (it answers the home folder itself when none is set).
fn downloads() -> PathBuf {
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
    std::process::Command::new("xdg-user-dir")
        .arg("DOWNLOAD")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| *path != home)
        .unwrap_or_else(|| home.join("Downloads"))
}

/// The address a light machine at `from` spoke as to reach `to`: its own
/// IPv6 address for an IPv6 one.
#[cfg(unix)]
fn source(net: &Net, from: Ipv4Addr, to: IpAddr) -> IpAddr {
    if to.is_ipv4() {
        return from.into();
    }
    net.nodes
        .iter()
        .position(|n| n.address == from)
        .and_then(|node| net.addresses6_of(node).into_iter().next())
        .map_or(from.into(), IpAddr::V6)
}
