//! What a network is made of: devices with ports, and the cables between
//! them. Machines are not devices: a machine plugs into a port (`Node::at`).
//!
//! The cables form a tree -- [`Topology::link`] refuses one that would close
//! a loop -- so from any device there is exactly one way to any other, and
//! [`Topology::toward`] is that way's first port. No spanning tree, no
//! learning: every machine's MAC is known, so a switch never has to guess.

use std::collections::BTreeMap;

use beatbox_reasons::{Reason, reason};
use serde_json::{Value, json};

use crate::frame;

/// One port of one device: where a machine plugs in, or where a cable ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct End {
    /// Index into [`Topology::devices`].
    pub device: usize,
    /// The device's own port number.
    pub port: u16,
}

/// The VLAN every port is in until it is told otherwise.
pub const DEFAULT_VLAN: u16 = 1;

/// What a port carries, and how it looks on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// One VLAN, untagged on the wire. A tagged frame is not accepted.
    Access(u16),
    /// Several VLANs, each tagged on the wire, except `native`, untagged.
    /// `allowed` empty carries every VLAN.
    Trunk {
        /// The VLAN untagged frames belong to.
        native: u16,
        /// The VLANs carried tagged; empty is all of them.
        allowed: Vec<u16>,
    },
}

impl Default for Mode {
    fn default() -> Self {
        Self::Access(DEFAULT_VLAN)
    }
}

impl Mode {
    /// Whether a frame of `vlan` may leave or enter through this port.
    pub fn carries(&self, vlan: u16) -> bool {
        match self {
            Self::Access(own) => *own == vlan,
            Self::Trunk { native, allowed } => {
                *native == vlan || allowed.is_empty() || allowed.contains(&vlan)
            }
        }
    }

    /// `frame` (untagged, of `vlan`) as it leaves through this port: as it
    /// is, tagged, or not at all.
    pub fn egress(&self, vlan: u16, frame: &[u8]) -> Option<Vec<u8>> {
        if !self.carries(vlan) {
            return None;
        }
        match self {
            Self::Trunk { native, .. } if *native != vlan => Some(frame::tag(frame, vlan)),
            _ => Some(frame.to_vec()),
        }
    }

    /// What arrives on the wire through this port: its VLAN and the frame
    /// untagged, or nothing when this port does not take it.
    pub fn ingress(&self, wire: &[u8]) -> Option<(u16, Vec<u8>)> {
        match (frame::untag(wire), self) {
            (Some(_), Self::Access(_)) => None,
            (Some((vlan, plain)), trunk) => trunk.carries(vlan).then_some((vlan, plain)),
            (None, Self::Access(own)) => Some((*own, wire.to_vec())),
            (None, Self::Trunk { native, .. }) => Some((*native, wire.to_vec())),
        }
    }
}

/// The VLAN ids a port may name.
pub const VLANS: std::ops::RangeInclusive<u16> = 1..=4094;

impl Mode {
    /// As `net.json` and the page write it.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Access(vlan) => json!({"mode": "access", "vlan": vlan}),
            Self::Trunk { native, allowed } => {
                json!({"mode": "trunk", "native": native, "allowed": allowed})
            }
        }
    }

    /// `{mode: access, vlan}` or `{mode: trunk, native, allowed}`, read and
    /// checked: VLAN ids from 1 to 4094.
    pub fn from_value(value: &Value) -> Result<Self, Reason> {
        let vlan = |key: &str, default: u16| -> Result<u16, Reason> {
            match value.get(key) {
                None | Some(Value::Null) => Ok(default),
                Some(raw) => raw
                    .as_u64()
                    .and_then(|v| u16::try_from(v).ok())
                    .filter(|v| VLANS.contains(v))
                    .ok_or_else(|| reason!("network.bad_vlan", vlan = raw)),
            }
        };
        match value
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("access")
        {
            "access" => Ok(Self::Access(vlan("vlan", DEFAULT_VLAN)?)),
            "trunk" => {
                let mut allowed = Vec::new();
                for raw in value
                    .get("allowed")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let id = raw
                        .as_u64()
                        .and_then(|v| u16::try_from(v).ok())
                        .filter(|v| VLANS.contains(v))
                        .ok_or_else(|| reason!("network.bad_vlan", vlan = raw))?;
                    if !allowed.contains(&id) {
                        allowed.push(id);
                    }
                }
                allowed.sort_unstable();
                Ok(Self::Trunk {
                    native: vlan("native", DEFAULT_VLAN)?,
                    allowed,
                })
            }
            other => Err(reason!("network.bad_rule", field = "mode", value = other)),
        }
    }
}

/// What a device is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The gateway: names, addresses, the firewall, the way out. What crosses
    /// it between two of its ports is judged by the firewall.
    Router,
    /// Carries frames between its ports by MAC, and judges nothing.
    Switch,
}

/// One device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// What the page and the rules call it.
    pub name: String,
    /// Router or switch.
    pub kind: Kind,
}

/// The devices and the cables between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// Every device; the first router is the gateway.
    pub devices: Vec<Device>,
    /// Each a pair of ports, either way round.
    pub cables: Vec<(End, End)>,
    /// Ports that are not in the default VLAN's access mode.
    pub modes: BTreeMap<End, Mode>,
}

impl Default for Topology {
    /// A network as it always was: one router, every machine on its ports.
    fn default() -> Self {
        Self {
            devices: vec![Device {
                name: "gw".into(),
                kind: Kind::Router,
            }],
            cables: Vec::new(),
            modes: BTreeMap::new(),
        }
    }
}

impl Topology {
    /// What `end` carries.
    pub fn mode(&self, end: End) -> Mode {
        self.modes.get(&end).cloned().unwrap_or_default()
    }

    /// A device by its name.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.devices.iter().position(|d| d.name == name)
    }

    /// `end` as the page and the API write it: `sw1:3`.
    pub fn end_text(&self, end: End) -> String {
        let name = self
            .devices
            .get(end.device)
            .map_or("?", |d| d.name.as_str());
        format!("{name}:{}", end.port)
    }

    /// `sw1:3`, or `sw1` alone (`port: None`), read.
    pub fn parse_end(&self, text: &str) -> Option<(usize, Option<u16>)> {
        match text.rsplit_once(':') {
            Some((name, port)) => Some((self.find(name)?, Some(port.parse().ok()?))),
            None => Some((self.find(text)?, None)),
        }
    }

    /// As `net.json` and the page see it: devices, cables and the ports that
    /// are not in the default access mode, every port written `name:port`.
    pub fn to_value(&self) -> Value {
        json!({
            "devices": self.devices.iter().map(|d| json!({
                "name": d.name,
                "kind": match d.kind { Kind::Router => "router", Kind::Switch => "switch" },
            })).collect::<Vec<_>>(),
            "cables": self.cables.iter().map(|(a, b)| json!([self.end_text(*a), self.end_text(*b)])).collect::<Vec<_>>(),
            "ports": self.modes.iter().map(|(end, mode)| (self.end_text(*end), mode.to_value())).collect::<serde_json::Map<_, _>>(),
        })
    }

    /// Read back what [`Topology::to_value`] wrote. A file from before
    /// devices existed has none: the network as it always was.
    pub fn from_value(value: &Value) -> Self {
        let mut topology = Self {
            devices: Vec::new(),
            cables: Vec::new(),
            modes: BTreeMap::new(),
        };
        for device in value["devices"].as_array().into_iter().flatten() {
            let kind = match device["kind"].as_str() {
                Some("switch") => Kind::Switch,
                _ => Kind::Router,
            };
            if let Some(name) = device["name"].as_str() {
                topology.devices.push(Device {
                    name: name.to_owned(),
                    kind,
                });
            }
        }
        if topology.router().is_none() {
            return Self::default();
        }
        let end = |topology: &Self, text: &Value| -> Option<End> {
            let (device, port) = topology.parse_end(text.as_str()?)?;
            Some(End {
                device,
                port: port?,
            })
        };
        for cable in value["cables"].as_array().into_iter().flatten() {
            if let (Some(a), Some(b)) = (end(&topology, &cable[0]), end(&topology, &cable[1])) {
                let _ = topology.link(a, b);
            }
        }
        for (text, mode) in value["ports"].as_object().into_iter().flatten() {
            if let (Some(at), Ok(mode)) = (end(&topology, &json!(text)), Mode::from_value(mode)) {
                topology.modes.insert(at, mode);
            }
        }
        topology
    }

    /// The router whose address is the gateway: the first one.
    pub fn router(&self) -> Option<usize> {
        self.devices.iter().position(|d| d.kind == Kind::Router)
    }

    /// The other end of the cable at `end`, if a cable is there.
    pub fn across(&self, end: End) -> Option<End> {
        self.cables.iter().find_map(|&(a, b)| {
            if a == end {
                Some(b)
            } else if b == end {
                Some(a)
            } else {
                None
            }
        })
    }

    /// The ports of `device` that have a cable.
    pub fn cabled(&self, device: usize) -> impl Iterator<Item = (u16, End)> + '_ {
        self.cables.iter().filter_map(move |&(a, b)| {
            if a.device == device {
                Some((a.port, b))
            } else if b.device == device {
                Some((b.port, a))
            } else {
                None
            }
        })
    }

    /// Whether `to` is reached from `at` without going back through `from`.
    fn reaches(&self, at: usize, to: usize, from: usize, depth: usize) -> bool {
        if at == to {
            return true;
        }
        depth < self.devices.len()
            && self.cabled(at).any(|(_, other)| {
                other.device != from && self.reaches(other.device, to, at, depth + 1)
            })
    }

    /// The port of `from` that leads to device `to`; `None` when they are
    /// not connected, or are the same device.
    pub fn toward(&self, from: usize, to: usize) -> Option<u16> {
        self.cabled(from)
            .find(|(_, other)| self.reaches(other.device, to, from, 0))
            .map(|(port, _)| port)
    }

    /// The port of `from` that leads to the port `to`: `to`'s own port when
    /// it is on `from`.
    pub fn toward_end(&self, from: usize, to: End) -> Option<u16> {
        if to.device == from {
            Some(to.port)
        } else {
            self.toward(from, to.device)
        }
    }

    /// The VLAN a machine plugged in at `at` reaches the router in: its
    /// untagged frames read by its own port, then carried cable by cable as
    /// each port puts them on the wire and the next reads them -- the very
    /// rules frames follow (`Mode::egress`, `Mode::ingress`). `None` when
    /// they never get there.
    pub fn router_vlan(&self, at: End) -> Option<u16> {
        self.vlan_toward(at, self.router()?)
    }

    /// The same, toward router `router`, any of them.
    pub fn vlan_toward(&self, at: End, router: usize) -> Option<u16> {
        let probe = frame::ethernet(
            frame::BROADCAST,
            [2, 0, 0, 0, 0, 1],
            frame::ETHER_ARP,
            &[0; 28],
        );
        let (mut vlan, _) = self.mode(at).ingress(&probe)?;
        let mut device = at.device;
        let mut steps = 0;
        while device != router {
            steps += 1;
            if steps > self.devices.len() {
                return None;
            }
            let out = End {
                device,
                port: self.toward(device, router)?,
            };
            let wire = self.mode(out).egress(vlan, &probe)?;
            let next = self.across(out)?;
            vlan = self.mode(next).ingress(&wire)?.0;
            device = next.device;
        }
        Some(vlan)
    }

    /// A cable from `a` to `b`, when both ports are free of cables (machines
    /// are the caller's to check) and it closes no loop.
    pub fn link(&mut self, a: End, b: End) -> Result<(), Reason> {
        if a.device >= self.devices.len() || b.device >= self.devices.len() {
            return Err(reason!("network.no_such_device"));
        }
        if a.device == b.device {
            return Err(reason!("network.loop"));
        }
        if self.across(a).is_some() || self.across(b).is_some() {
            return Err(reason!("network.port_taken"));
        }
        if self.reaches(a.device, b.device, usize::MAX, 0) {
            return Err(reason!("network.loop"));
        }
        self.cables.push((a, b));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end(device: usize, port: u16) -> End {
        End { device, port }
    }

    #[test]
    fn a_machine_reaches_the_router_in_the_vlan_its_frames_end_up_in() {
        let mut topo = Topology::default();
        topo.devices.push(Device {
            name: "sw1".into(),
            kind: Kind::Switch,
        });
        topo.link(end(0, 1), end(1, 0)).unwrap();
        // Everything in VLAN 1.
        assert_eq!(topo.router_vlan(end(1, 1)), Some(1));
        assert_eq!(topo.router_vlan(end(0, 0)), Some(1));
        // VLAN 10 at the machine, VLAN 1 on the way up: nowhere.
        topo.modes.insert(end(1, 1), Mode::Access(10));
        assert_eq!(topo.router_vlan(end(1, 1)), None);
        // A trunk carrying 10 up to the router: there, in 10.
        let trunk = Mode::Trunk {
            native: 1,
            allowed: vec![10],
        };
        topo.modes.insert(end(1, 0), trunk.clone());
        topo.modes.insert(end(0, 1), trunk);
        assert_eq!(topo.router_vlan(end(1, 1)), Some(10));
        // A switch with no cable to the router.
        topo.cables.clear();
        assert_eq!(topo.router_vlan(end(1, 1)), None);
    }

    /// gw ── sw1 ── sw2, and sw3 on gw too.
    fn tree() -> Topology {
        let mut topology = Topology::default();
        for name in ["sw1", "sw2", "sw3"] {
            topology.devices.push(Device {
                name: name.into(),
                kind: Kind::Switch,
            });
        }
        topology.link(end(0, 10), end(1, 0)).unwrap();
        topology.link(end(1, 7), end(2, 0)).unwrap();
        topology.link(end(0, 11), end(3, 0)).unwrap();
        topology
    }

    #[test]
    fn the_way_from_one_device_to_another_is_its_first_port() {
        let topology = tree();
        assert_eq!(
            topology.toward(0, 2),
            Some(10),
            "gw reaches sw2 through sw1"
        );
        assert_eq!(topology.toward(2, 0), Some(0));
        assert_eq!(topology.toward(2, 3), Some(0), "sw2 to sw3 goes back up");
        assert_eq!(topology.toward(1, 2), Some(7));
        assert_eq!(topology.toward(0, 0), None);
        assert_eq!(topology.toward_end(1, end(1, 3)), Some(3));
        assert_eq!(topology.router(), Some(0));
        assert_eq!(topology.across(end(1, 7)), Some(end(2, 0)));
    }

    fn frame(vlan_tag: Option<u16>) -> Vec<u8> {
        let plain = frame::ethernet(
            [2, 0, 0, 0, 0, 2],
            [2, 0, 0, 0, 0, 1],
            frame::ETHER_IPV4,
            &[0x45; 20],
        );
        vlan_tag.map_or(plain.clone(), |vlan| frame::tag(&plain, vlan))
    }

    #[test]
    fn an_access_port_carries_its_vlan_untagged_and_refuses_tags() {
        let access = Mode::Access(10);
        assert_eq!(access.ingress(&frame(None)), Some((10, frame(None))));
        assert_eq!(
            access.ingress(&frame(Some(10))),
            None,
            "a tag on an access port is not taken"
        );
        assert_eq!(access.egress(10, &frame(None)), Some(frame(None)));
        assert_eq!(access.egress(20, &frame(None)), None);
    }

    #[test]
    fn a_trunk_tags_all_but_its_native_and_only_what_it_allows() {
        let trunk = Mode::Trunk {
            native: 1,
            allowed: vec![10, 20],
        };
        assert_eq!(trunk.egress(10, &frame(None)), Some(frame(Some(10))));
        assert_eq!(
            trunk.egress(1, &frame(None)),
            Some(frame(None)),
            "native goes untagged"
        );
        assert_eq!(trunk.egress(30, &frame(None)), None);
        assert_eq!(trunk.ingress(&frame(Some(20))), Some((20, frame(None))));
        assert_eq!(trunk.ingress(&frame(Some(30))), None);
        assert_eq!(trunk.ingress(&frame(None)), Some((1, frame(None))));
        let every = Mode::Trunk {
            native: 1,
            allowed: Vec::new(),
        };
        assert!(every.carries(4094));
    }

    #[test]
    fn ports_are_written_and_read_by_name() {
        let topology = tree();
        assert_eq!(topology.end_text(end(2, 3)), "sw2:3");
        assert_eq!(topology.parse_end("sw2:3"), Some((2, Some(3))));
        assert_eq!(topology.parse_end("sw2"), Some((2, None)));
        assert_eq!(topology.parse_end("nope:1"), None);
        assert_eq!(topology.parse_end("sw2:x"), None);
    }

    #[test]
    fn a_topology_reads_back_as_it_was_written() {
        let mut topology = tree();
        topology.modes.insert(end(1, 3), Mode::Access(10));
        topology.modes.insert(
            end(1, 7),
            Mode::Trunk {
                native: 1,
                allowed: vec![10, 20],
            },
        );
        let again = Topology::from_value(&topology.to_value());
        assert_eq!(again, topology);
        // A file from before devices: the network as it always was.
        assert_eq!(
            Topology::from_value(&serde_json::Value::Null),
            Topology::default()
        );
    }

    #[test]
    fn a_port_mode_is_checked_as_it_is_read() {
        let read = |value: serde_json::Value| Mode::from_value(&value);
        assert_eq!(
            read(json!({"mode": "access", "vlan": 20})),
            Ok(Mode::Access(20))
        );
        assert_eq!(
            read(json!({"mode": "trunk", "allowed": [20, 10, 20]})),
            Ok(Mode::Trunk {
                native: 1,
                allowed: vec![10, 20]
            })
        );
        assert_eq!(
            read(json!({"mode": "access", "vlan": 4095}))
                .unwrap_err()
                .code,
            "network.bad_vlan"
        );
        assert_eq!(
            read(json!({"mode": "trunk", "allowed": [0]}))
                .unwrap_err()
                .code,
            "network.bad_vlan"
        );
        assert_eq!(
            read(json!({"mode": "hub"})).unwrap_err().code,
            "network.bad_rule"
        );
    }

    #[test]
    fn a_cable_that_closes_a_loop_or_reuses_a_port_is_refused() {
        let mut topology = tree();
        let code = |result: Result<(), Reason>| result.unwrap_err().code;
        assert_eq!(code(topology.link(end(2, 1), end(3, 1))), "network.loop");
        assert_eq!(code(topology.link(end(1, 1), end(1, 2))), "network.loop");
        assert_eq!(
            code(topology.link(end(1, 7), end(3, 5))),
            "network.port_taken"
        );
        assert_eq!(
            code(topology.link(end(1, 1), end(9, 0))),
            "network.no_such_device"
        );
        assert_eq!(topology.cables.len(), 3, "nothing was added");
    }
}
