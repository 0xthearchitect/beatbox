//! Moving a network to another range, as one decision.
//!
//! The range is written down in the nodes' addresses, in the names, in the
//! rules that name a range or an address, and in the settings a reset goes
//! back to. [`plan`] works out where every one of them goes, with no I/O,
//! so the preview and the change are the same code and cannot disagree;
//! [`Plan::apply`] and [`Plan::settings`] then carry it out. A node's MAC is
//! never touched: the machine holds it in its own cable, and the switch
//! refuses any other.

use std::net::Ipv4Addr;

use beatbox_engine::{Data, data};
use beatbox_reasons::{Reason, reason};
use serde_json::{Value, json};

use crate::switch::{Baseline, Net, Node, parse_cidr, within};

/// One thing that moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// A node's or a name's own name.
    pub what: String,
    /// Where it was.
    pub from: Ipv4Addr,
    /// Where it goes.
    pub to: Ipv4Addr,
}

/// One end of a firewall rule that is rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleEnd {
    /// `r6`, or empty for a rule in the settings a reset goes back to.
    pub id: String,
    /// `source` or `destination`.
    pub end: &'static str,
    /// As written.
    pub from: String,
    /// As it will be written.
    pub to: String,
}

/// Where everything goes.
#[derive(Debug, Clone)]
pub struct Plan {
    from: (Ipv4Addr, u8),
    to: (Ipv4Addr, u8),
    /// Each node's old address and its new one, in the nodes' order.
    pub nodes: Vec<Moved>,
    /// The names whose address moves.
    pub names: Vec<Moved>,
    /// The rule ends that move.
    pub rules: Vec<RuleEnd>,
    /// Addresses written somewhere that the new range has no place for, left
    /// as they are: they stay pointing where they pointed.
    pub kept: Vec<String>,
}

fn host_ok(address: Ipv4Addr, range: (Ipv4Addr, u8)) -> bool {
    let first = u32::from(range.0);
    let last = first | !(u32::MAX.checked_shl(32 - u32::from(range.1)).unwrap_or(0));
    let at = u32::from(address);
    within(address, range.0, range.1) && at > first + 1 && at < last
}

/// `address` at the same distance from the start of the new range as it was
/// from the old, if that is inside the new one.
fn shift(address: Ipv4Addr, from: (Ipv4Addr, u8), to: (Ipv4Addr, u8)) -> Option<Ipv4Addr> {
    let tail = u32::from(address).checked_sub(u32::from(from.0))?;
    let moved = Ipv4Addr::from(u32::from(to.0).checked_add(tail)?);
    within(moved, to.0, to.1).then_some(moved)
}

/// Where every node, name and rule goes when `nodes` move from the range of
/// `old` to `to`; a reason, and nothing else, when they cannot.
pub fn plan(old: &Baseline, nodes: &[Node], to: (Ipv4Addr, u8)) -> Result<Plan, Reason> {
    let from = (old.network, old.prefix);
    if from == to {
        return Err(reason!("network.renumber_same"));
    }
    let gateway = Ipv4Addr::from(u32::from(to.0) + 1);
    // Whoever's tail still fits keeps it; the rest take the first free
    // address from .10 up, in their old order.
    let mut taken: Vec<Ipv4Addr> = Vec::new();
    let mut assigned: Vec<Option<Ipv4Addr>> = nodes
        .iter()
        .map(|node| {
            let kept = shift(node.address, from, to)
                .filter(|moved| host_ok(*moved, to) && !taken.contains(moved));
            if let Some(moved) = kept {
                taken.push(moved);
            }
            kept
        })
        .collect();
    let last = u32::from(to.0) | !(u32::MAX.checked_shl(32 - u32::from(to.1)).unwrap_or(0));
    let mut next = u32::from(to.0) + 10;
    for slot in assigned.iter_mut().filter(|slot| slot.is_none()) {
        let free = (next..last)
            .map(Ipv4Addr::from)
            .find(|candidate| host_ok(*candidate, to) && !taken.contains(candidate));
        let Some(free) = free else {
            return Err(reason!("network.full", cidr = format!("{}/{}", to.0, to.1)));
        };
        next = u32::from(free) + 1;
        taken.push(free);
        *slot = Some(free);
    }
    let moved: Vec<Moved> = nodes
        .iter()
        .zip(assigned)
        .map(|(node, to)| Moved {
            what: node.name.clone(),
            from: node.address,
            to: to.expect("every node was given an address"),
        })
        .collect();
    let mut plan = Plan {
        from,
        to,
        nodes: moved,
        names: Vec::new(),
        rules: Vec::new(),
        kept: Vec::new(),
    };
    let old_gateway = old.gateway();
    plan.names = old
        .dns
        .iter()
        .filter_map(|(name, address)| {
            let to = plan.address(*address, old_gateway, gateway)?;
            (to != *address).then(|| Moved {
                what: name.clone(),
                from: *address,
                to,
            })
        })
        .collect();
    for rule in &old.rules {
        for (end, spec) in [("source", &rule.source), ("destination", &rule.destination)] {
            if let Some(to) = plan.spec(spec) {
                plan.rules.push(RuleEnd {
                    id: rule.id.clone(),
                    end,
                    from: (*spec).clone(),
                    to,
                });
            }
        }
    }
    plan.kept = plan.leftovers(old);
    Ok(plan)
}

impl Plan {
    /// The new range.
    pub fn range(&self) -> (Ipv4Addr, u8) {
        self.to
    }

    /// The new gateway.
    pub fn gateway(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.to.0) + 1)
    }

    /// Where one address goes: a node's, the gateway's, or the same distance
    /// into the new range. `None`: it was not in the old range, or has no
    /// place in the new one.
    fn address(
        &self,
        address: Ipv4Addr,
        old_gateway: Ipv4Addr,
        gateway: Ipv4Addr,
    ) -> Option<Ipv4Addr> {
        if let Some(node) = self.nodes.iter().find(|node| node.from == address) {
            return Some(node.to);
        }
        if address == old_gateway {
            return Some(gateway);
        }
        if !within(address, self.from.0, self.from.1) {
            return None;
        }
        shift(address, self.from, self.to).filter(|moved| host_ok(*moved, self.to))
    }

    /// A rule's end, if it names a range or an address that moves: the old
    /// range itself becomes the new; an address becomes its node's, the
    /// gateway's or the same distance in; a sub-range keeps its length.
    fn spec(&self, spec: &str) -> Option<String> {
        let (address, length) = parse_cidr(spec)?;
        let written_long = spec.contains('/');
        if (address, length) == self.from {
            return Some(format!("{}/{}", self.to.0, self.to.1));
        }
        if length < self.from.1 || !within(address, self.from.0, self.from.1) {
            return None;
        }
        let old_gateway = Ipv4Addr::from(u32::from(self.from.0) + 1);
        if length == 32 {
            let moved = self.address(address, old_gateway, self.gateway())?;
            return Some(if written_long {
                format!("{moved}/32")
            } else {
                moved.to_string()
            });
        }
        if length < self.to.1 {
            return None;
        }
        let base = shift(address, self.from, self.to)?;
        Some(format!("{base}/{length}"))
    }

    /// Addresses written in names and rules inside the old range that the new
    /// one has no place for.
    fn leftovers(&self, old: &Baseline) -> Vec<String> {
        let old_gateway = old.gateway();
        let gateway = self.gateway();
        let mut kept: Vec<String> = old
            .dns
            .iter()
            .filter(|(_, address)| {
                within(**address, self.from.0, self.from.1)
                    && self.address(**address, old_gateway, gateway).is_none()
            })
            .map(|(name, _)| name.clone())
            .collect();
        for rule in &old.rules {
            for spec in [&rule.source, &rule.destination] {
                let inside = parse_cidr(spec)
                    .is_some_and(|(a, l)| l >= self.from.1 && within(a, self.from.0, self.from.1));
                if inside && self.spec(spec).is_none() && !kept.contains(spec) {
                    kept.push(spec.clone());
                }
            }
        }
        kept
    }

    /// Carry it out on a running or stopped network: its range, the nodes'
    /// addresses (their MACs stay), the names and rules as they are now.
    /// Conversations remembered under the old addresses are forgotten.
    pub fn apply(&self, net: &mut Net) {
        let old_gateway = net.live.gateway();
        let gateway = self.gateway();
        for node in &mut net.nodes {
            if let Some(moved) = self.nodes.iter().find(|moved| moved.what == node.name) {
                node.address = moved.to;
            }
        }
        for baseline in [&mut net.baseline, &mut net.live] {
            baseline.network = self.to.0;
            baseline.prefix = self.to.1;
        }
        let names: Vec<(String, Ipv4Addr)> = net
            .live
            .dns
            .iter()
            .map(|(name, address)| {
                (
                    name.clone(),
                    self.address(*address, old_gateway, gateway)
                        .unwrap_or(*address),
                )
            })
            .collect();
        net.live.dns = names.into_iter().collect();
        for rule in &mut net.live.rules {
            for end in [&mut rule.source, &mut rule.destination] {
                if let Some(moved) = self.spec(end) {
                    *end = moved;
                }
            }
        }
        net.forget_conversations();
    }

    /// Carry it out for one VLAN's network on the router (`Net::interfaces`)
    /// once its new range is set: the machines in it keep their tail, and
    /// the names and rules that pointed into it follow. Not the settings: a
    /// VLAN's network is not among them.
    pub fn apply_to_vlan(&self, net: &mut Net) {
        let old_gateway = Ipv4Addr::from(u32::from(self.from.0) + 1);
        let gateway = self.gateway();
        let moves: Vec<(String, Ipv4Addr, Ipv4Addr)> = self
            .nodes
            .iter()
            .map(|moved| (moved.what.clone(), moved.from, moved.to))
            .collect();
        for (name, from, to) in moves {
            net.move_node(&name, from, to);
        }
        for address in net.live.dns.values_mut() {
            if let Some(moved) = self.address(*address, old_gateway, gateway) {
                *address = moved;
            }
        }
        for rule in &mut net.live.rules {
            for end in [&mut rule.source, &mut rule.destination] {
                if let Some(moved) = self.spec(end) {
                    *end = moved;
                }
            }
        }
        net.forget_conversations();
    }

    /// The settings a network is made from, moved: its range, and the names
    /// and rules among them. What a reset goes back to.
    pub fn settings(&self, old: &Data) -> Data {
        let mut settings = old.clone();
        let old_gateway = Ipv4Addr::from(u32::from(self.from.0) + 1);
        settings.insert("cidr".into(), json!(format!("{}/{}", self.to.0, self.to.1)));
        if let Some(Value::Object(dns)) = settings.get_mut("dns") {
            for address in dns.values_mut() {
                let moved = address
                    .as_str()
                    .and_then(|text| text.parse::<Ipv4Addr>().ok())
                    .and_then(|a| self.address(a, old_gateway, self.gateway()));
                if let Some(moved) = moved {
                    *address = json!(moved.to_string());
                }
            }
        }
        if let Some(Value::Array(rules)) = settings
            .get_mut("firewall")
            .and_then(|f| f.get_mut("rules"))
        {
            for rule in rules {
                for end in ["source", "destination"] {
                    let moved = rule
                        .get(end)
                        .and_then(Value::as_str)
                        .and_then(|spec| self.spec(spec));
                    if let Some(moved) = moved {
                        rule[end] = json!(moved);
                    }
                }
            }
        }
        settings
    }

    /// The plan as data, for a page to turn into sentences.
    pub fn describe(&self, old_settings: &Data) -> Data {
        let moved = |list: &[Moved]| -> Vec<Value> {
            list.iter()
                .map(|each| json!({"what": each.what, "from": each.from.to_string(), "to": each.to.to_string()}))
                .collect()
        };
        data!({
            "from": format!("{}/{}", self.from.0, self.from.1),
            "to": format!("{}/{}", self.to.0, self.to.1),
            "gateway": self.gateway().to_string(),
            "nodes": moved(&self.nodes),
            "names": moved(&self.names),
            "rules": self.rules.iter().map(|rule| json!({
                "id": rule.id, "end": rule.end, "from": rule.from, "to": rule.to,
            })).collect::<Vec<_>>(),
            "kept": self.kept,
            "settings": self.settings(old_settings),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::frame::node_mac;
    use crate::switch::{Rule, Traffic, Verdict};

    fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
        Ipv4Addr::new(a, b, c, d)
    }

    fn rule(source: &str, destination: &str) -> Rule {
        Rule {
            id: "r1".into(),
            verdict: Verdict::Allow,
            source: source.into(),
            destination: destination.into(),
            protocol: None,
            ports: None,
            ..Rule::default()
        }
    }

    fn lab(rules: Vec<Rule>) -> (Baseline, Vec<Node>) {
        let mut dns = BTreeMap::new();
        dns.insert("portal.local".to_owned(), ip(10, 50, 20, 11));
        dns.insert("fileserver".to_owned(), ip(10, 50, 20, 20));
        dns.insert("elsewhere".to_owned(), ip(192, 168, 0, 9));
        let baseline = Baseline {
            network: ip(10, 50, 20, 0),
            prefix: 24,
            default_allow: false,
            rules,
            answers: true,
            groups: BTreeMap::new(),
            services: BTreeMap::new(),
            dns,
            records: Vec::new(),
            domain: "local".into(),
            traffic: Traffic::default(),
            internet: false,
            names: Vec::new(),
        };
        let nodes = [("pc1", 10), ("web", 11), ("srv", 12)]
            .into_iter()
            .map(|(name, last)| {
                let address = ip(10, 50, 20, last);
                Node {
                    name: name.into(),
                    address,
                    mac: node_mac(address),
                    port: PathBuf::from(name),
                    at: crate::topology::End {
                        device: 0,
                        port: u16::from(last),
                    },
                }
            })
            .collect();
        (baseline, nodes)
    }

    #[test]
    fn nodes_keep_their_tail_in_the_new_range() {
        let (old, nodes) = lab(Vec::new());
        let plan = plan(&old, &nodes, (ip(10, 60, 0, 0), 24)).unwrap();
        let to: Vec<_> = plan.nodes.iter().map(|m| m.to).collect();
        assert_eq!(
            to,
            [ip(10, 60, 0, 10), ip(10, 60, 0, 11), ip(10, 60, 0, 12)]
        );
        assert_eq!(plan.gateway(), ip(10, 60, 0, 1));
    }

    #[test]
    fn a_tail_that_does_not_fit_takes_the_first_free_address_in_order() {
        let (old, nodes) = lab(Vec::new());
        // A /28 holds .1 to .14: .10 .11 and .12 fit; a /29 (.1 to .6) holds none.
        let small = plan(&old, &nodes, (ip(10, 70, 0, 0), 29));
        assert!(
            matches!(small, Err(ref why) if why.code == "network.full"),
            "{small:?}"
        );
        let tight = plan(&old, &nodes, (ip(10, 70, 0, 0), 28)).unwrap();
        assert_eq!(
            tight.nodes.iter().map(|m| m.to).collect::<Vec<_>>(),
            [ip(10, 70, 0, 10), ip(10, 70, 0, 11), ip(10, 70, 0, 12)]
        );
        // Tails .10 .11 .12 against a range starting at .8: they land on .18...
        let (mut moved, mut nodes) = lab(Vec::new());
        moved.network = ip(10, 50, 20, 0);
        nodes[2].address = ip(10, 50, 20, 200);
        let shifted = plan(&moved, &nodes, (ip(10, 90, 0, 0), 25)).unwrap();
        // .200 is outside a /25: it takes the first free from .10, after .10 and .11 kept.
        assert_eq!(shifted.nodes[2].to, ip(10, 90, 0, 12));
    }

    #[test]
    fn the_same_range_is_refused() {
        let (old, nodes) = lab(Vec::new());
        let same = plan(&old, &nodes, (ip(10, 50, 20, 0), 24));
        assert!(matches!(same, Err(ref why) if why.code == "network.renumber_same"));
    }

    #[test]
    fn names_follow_their_node_and_the_gateway_and_leave_the_rest() {
        let (mut old, nodes) = lab(Vec::new());
        old.dns.insert("gw".into(), ip(10, 50, 20, 1));
        let plan = plan(&old, &nodes, (ip(10, 60, 0, 0), 24)).unwrap();
        let by_name = |name: &str| plan.names.iter().find(|m| m.what == name).map(|m| m.to);
        assert_eq!(by_name("portal.local"), Some(ip(10, 60, 0, 11)));
        assert_eq!(by_name("gw"), Some(ip(10, 60, 0, 1)));
        assert_eq!(
            by_name("fileserver"),
            Some(ip(10, 60, 0, 20)),
            "nobody is plugged in there, but it is in the range"
        );
        assert_eq!(
            by_name("elsewhere"),
            None,
            "outside the old range: not ours to move"
        );
    }

    #[test]
    fn rules_move_with_a_range_an_address_or_a_part_of_one() {
        let (old, nodes) = lab(vec![
            rule("10.50.20.0/24", "pc1"),
            rule("10.50.20.11", "gateway"),
            rule("10.50.20.0/28", "any"),
            rule("internet", "10.50.20.12/32"),
            rule("192.168.0.0/16", "10.0.0.0/8"),
        ]);
        let plan = plan(&old, &nodes, (ip(10, 60, 0, 0), 24)).unwrap();
        let ends: Vec<(String, String)> = plan
            .rules
            .iter()
            .map(|r| (r.from.clone(), r.to.clone()))
            .collect();
        assert_eq!(
            ends,
            [
                ("10.50.20.0/24".to_owned(), "10.60.0.0/24".to_owned()),
                ("10.50.20.11".to_owned(), "10.60.0.11".to_owned()),
                ("10.50.20.0/28".to_owned(), "10.60.0.0/28".to_owned()),
                ("10.50.20.12/32".to_owned(), "10.60.0.12/32".to_owned()),
            ]
        );
    }

    #[test]
    fn settings_and_a_running_net_are_moved_by_the_same_plan() {
        let (old, nodes) = lab(vec![rule("10.50.20.0/24", "pc1")]);
        let plan = plan(&old, &nodes, (ip(10, 60, 0, 0), 24)).unwrap();
        let settings = data!({
            "cidr": "10.50.20.0/24",
            "firewall": {"default": "deny", "rules": [{"action": "allow", "source": "10.50.20.0/24", "destination": "pc1"}]},
            "dns": {"portal.local": "10.50.20.11", "elsewhere": "192.168.0.9"},
            "traffic": {"latency_ms": 5},
        });
        let moved = plan.settings(&settings);
        assert_eq!(moved["cidr"], "10.60.0.0/24");
        assert_eq!(moved["firewall"]["rules"][0]["source"], "10.60.0.0/24");
        assert_eq!(moved["firewall"]["default"], "deny");
        assert_eq!(moved["dns"]["portal.local"], "10.60.0.11");
        assert_eq!(moved["dns"]["elsewhere"], "192.168.0.9");
        assert_eq!(moved["traffic"], settings["traffic"]);

        let mut net = Net::new(old.clone());
        net.nodes = nodes.clone();
        let macs: Vec<_> = net.nodes.iter().map(|n| n.mac).collect();
        plan.apply(&mut net);
        assert_eq!(net.live.network, ip(10, 60, 0, 0));
        assert_eq!(net.live.gateway(), ip(10, 60, 0, 1));
        assert_eq!(net.nodes[1].address, ip(10, 60, 0, 11));
        assert_eq!(
            net.nodes.iter().map(|n| n.mac).collect::<Vec<_>>(),
            macs,
            "the machines' cables stay valid"
        );
        assert_eq!(net.live.dns["portal.local"], ip(10, 60, 0, 11));
        assert_eq!(net.live.rules[0].source, "10.60.0.0/24");
        assert_eq!(net.baseline.network, ip(10, 60, 0, 0));
    }

    #[test]
    fn a_name_left_where_the_new_range_has_no_place_is_said() {
        let (mut old, nodes) = lab(Vec::new());
        old.dns.insert("far".into(), ip(10, 50, 20, 200));
        let plan = plan(&old, &nodes, (ip(10, 60, 0, 0), 25)).unwrap();
        assert_eq!(plan.kept, ["far"]);
    }
}
