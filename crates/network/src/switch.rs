//! The switch: what a network *is* while it runs.
//!
//! One UNIX datagram socket, one thread, one frame per datagram (QEMU's
//! `-netdev dgram`). Every frame passes one place, so the firewall, traffic
//! shaping and observation are a few lines, and none of it needs privilege:
//! no TAP, bridge, nftables or `tc`.
//!
//! [`Net`] decides and the thread only carries: every rule is a method on
//! `Net` that a test can call with a byte slice.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use beatbox_engine::{Data, data};

use crate::frame::{self, BROADCAST, Flow, GATEWAY_MAC, Mac};
use crate::topology::{DEFAULT_VLAN, Device, End, Kind, Mode, Topology};

/// One thing cabled to this network.
#[derive(Debug, Clone)]
pub struct Node {
    /// What rules and DNS call it.
    pub name: String,
    /// The one address it may send from.
    pub address: Ipv4Addr,
    /// The one Ethernet address it may send from.
    pub mac: Mac,
    /// Its end of the cable: the path its frames arrive from.
    pub port: PathBuf,
    /// The device port it is plugged into.
    pub at: End,
}

/// What a rule does to what it fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verdict {
    /// Let it through.
    #[default]
    Allow,
    /// Drop it: whoever sent it waits, and hears nothing.
    Deny,
    /// Drop it and say so: an ICMP "unreachable", so the sender hears "no"
    /// at once instead of waiting.
    Reject,
}

impl Verdict {
    /// `allow`, `deny` or `reject`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Reject => "reject",
        }
    }
}

/// One firewall rule. First match wins.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rule {
    /// `r1`, `r2`, ... in the order they were made.
    pub id: String,
    /// What it does.
    pub verdict: Verdict,
    /// Only conversations that begin: an answer is not its business.
    pub only_new: bool,
    /// A node, an address, a CIDR, `vlan:N`, `group:name`, `gateway`,
    /// `internet`, or `any`.
    pub source: String,
    /// The same.
    pub destination: String,
    /// `None` for any.
    pub protocol: Option<u8>,
    /// The first and last destination port; `None` for any. Ignored for ICMP.
    pub ports: Option<(u16, u16)>,
    /// The ICMP type (8: a ping); `None` for any.
    pub icmp_type: Option<u8>,
    /// A frame that is not IP, by EtherType. Such a rule judges nothing
    /// else, and only such rules judge those frames.
    pub ethertype: Option<u16>,
    /// The service it was written with, for the page to say.
    pub service: Option<String>,
    /// How many new conversations it lets through in a while; past that,
    /// what it does instead. Only an allowing rule has one.
    pub limit: Option<Limit>,
    /// Only IPv4 (4) or only IPv6 (6); `None` for both.
    pub family: Option<u8>,
}

/// The ICMPv6 type that means what an ICMP (IPv4) type does: a rule says
/// "ping" once, for both.
fn icmp6_of(kind: u8) -> u8 {
    match kind {
        8 => frame::ECHO6_REQUEST,
        0 => frame::ECHO6_REPLY,
        3 => frame::ICMPV6_UNREACHABLE,
        11 => frame::ICMPV6_TIME_EXCEEDED,
        other => other,
    }
}

/// The packet of an IP frame, without the Ethernet header or anything
/// after the packet: IPv4 by its total length, IPv6 by its payload length.
fn packet_alone(bytes: &[u8]) -> &[u8] {
    if frame::ethertype(bytes) != Some(frame::ETHER_IPV6) {
        return ip_alone(bytes);
    }
    let ip = bytes.get(14..).unwrap_or_default();
    let length = ip.get(4..6).map_or(ip.len(), |l| {
        40 + usize::from(u16::from_be_bytes([l[0], l[1]]))
    });
    &ip[..length.min(ip.len())]
}

/// Its flow, when a frame is IPv4 or IPv6.
fn flow_of(bytes: &[u8]) -> Option<Flow> {
    frame::ipv4(bytes)
        .map(|p| p.flow)
        .or_else(|| frame::ipv6(bytes).map(|p| p.flow))
}

/// A rule's pace: at most `count` new conversations every `per_ms`, from
/// each machine or from all of them together; past that, `over`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    /// How many.
    pub count: u32,
    /// In how long: a second, a minute or an hour, in ms.
    pub per_ms: u64,
    /// Counted for each machine, or for the rule as a whole.
    pub each: bool,
    /// Block or reject what comes past it.
    pub over: Verdict,
}

/// A name for a protocol and ports, for rules to be written with.
#[derive(Debug, Clone, PartialEq)]
pub struct Service {
    /// The IP protocol.
    pub protocol: u8,
    /// The first and last port; `None` for any.
    pub ports: Option<(u16, u16)>,
}

/// What the network does to every frame it carries.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Traffic {
    /// Added to every frame, each way.
    pub latency_ms: f64,
    /// Up to this much more or less, chosen per frame.
    pub jitter_ms: f64,
    /// The share of frames that never arrive, 0 to 100.
    pub loss_percent: f64,
    /// What each node's cable carries, towards the node. 0 is unlimited.
    pub bandwidth_kbps: f64,
}

impl Traffic {
    /// This, then `next` after it: delays add up, losses compound, and the
    /// narrowest bandwidth is what gets through.
    pub fn then(self, next: Self) -> Self {
        let narrow = match (self.bandwidth_kbps, next.bandwidth_kbps) {
            (a, b) if a > 0.0 && b > 0.0 => a.min(b),
            (a, b) => a.max(b),
        };
        Self {
            latency_ms: self.latency_ms + next.latency_ms,
            jitter_ms: self.jitter_ms + next.jitter_ms,
            loss_percent: 100.0 - (100.0 - self.loss_percent) * (100.0 - next.loss_percent) / 100.0,
            bandwidth_kbps: narrow,
        }
    }

    /// Whether it does anything at all.
    pub fn is_perfect(&self) -> bool {
        *self == Self::default()
    }
}

/// One step of a packet's way through the network: a device it entered,
/// at which port, in which VLAN, and how long it had been travelling.
#[derive(Debug, Clone, PartialEq)]
pub struct Hop {
    /// The device's name.
    pub device: String,
    /// `router` or `switch`.
    pub kind: &'static str,
    /// The port it came in by.
    pub port: u16,
    /// The VLAN it came in on.
    pub vlan: u16,
    /// Its TTL when it arrived, at a router.
    pub ttl: Option<u8>,
    /// The cables' delay so far, in ms.
    pub ms: f64,
}

/// The way one conversation's packet went, and where it ended
/// (`Net::flows`): the first packet's, and again whenever the outcome
/// changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Trail {
    /// The conversation as its first packet said it.
    key: Option<Flow>,
    /// The packet being followed now: `key`, or what NAT made of it.
    flow: Option<Flow>,
    /// Where a router changed its addresses, when one did.
    pub nat: Option<NatStep>,
    /// The devices it went through, in order.
    pub hops: Vec<Hop>,
    /// What the firewall said and by which rule (`None`: the default, or
    /// let through as an answer); `None` when nothing judged it.
    pub judged: Option<(Verdict, Option<String>)>,
    /// Stopped by its rule's limit, not by the rule itself.
    pub limited: bool,
    /// The machine it reached, `internet`, or `None`: it went nowhere.
    pub reached: Option<String>,
    /// The whole delay to where it reached, in ms.
    pub ms: f64,
}

impl Trail {
    fn value(&self) -> serde_json::Value {
        serde_json::json!({
            "hops": self.hops.iter().map(|hop| serde_json::json!({
                "device": hop.device, "kind": hop.kind, "port": hop.port, "vlan": hop.vlan,
                "ttl": hop.ttl, "ms": hop.ms,
            })).collect::<Vec<_>>(),
            "verdict": self.judged.as_ref().map(|(verdict, _)| verdict.name()),
            "rule": self.judged.as_ref().and_then(|(_, rule)| rule.clone()),
            "limited": self.limited,
            "reached": self.reached,
            "ms": self.ms,
            "nat": self.nat.as_ref().map(|nat| serde_json::json!({
                "router": nat.router, "kind": nat.kind,
                "from": format!("{}:{}", nat.from.0, nat.from.1),
                "to": format!("{}:{}", nat.to.0, nat.to.1),
            })),
        })
    }
}

/// Where a router changed a packet's addresses (`RouterConf::hidden`,
/// `RouterConf::forwards`).
#[derive(Debug, Clone, PartialEq)]
pub struct NatStep {
    /// The router's name.
    pub router: String,
    /// `hide`: the source became the router; `forward`: the destination
    /// became a machine behind it; `closed`: it was stopped at a hidden
    /// network's door.
    pub kind: &'static str,
    /// The address and port before.
    pub from: (Ipv4Addr, u16),
    /// And after.
    pub to: (Ipv4Addr, u16),
}

/// A router's door into one of its machines: what arrives at the router on
/// `protocol` and `port` goes to machine `to`, port `to_port`.
#[derive(Debug, Clone, PartialEq)]
pub struct Forward {
    /// `f1`, `f2`, ... on this router.
    pub id: String,
    /// TCP or UDP.
    pub protocol: u8,
    /// On the router.
    pub port: u16,
    /// The machine's name.
    pub to: String,
    /// On the machine.
    pub to_port: u16,
    /// Conversations it let in since the network started, and when the
    /// last one came.
    pub uses: u64,
    /// See `uses`.
    pub last: Option<Instant>,
}

/// One end of a NAT mapping: protocol, one address and port, the other.
type NatKey = (u8, Ipv4Addr, u16, Ipv4Addr, u16);

/// `flow` with a new source or destination; a ping's identifier is both its ports.
fn moved(
    flow: Flow,
    source: Option<(Ipv4Addr, u16)>,
    destination: Option<(Ipv4Addr, u16)>,
) -> Flow {
    let mut next = flow;
    if let Some((address, port)) = source {
        next.source = address.into();
        next.source_port = port;
    }
    if let Some((address, port)) = destination {
        next.destination = address.into();
        next.destination_port = port;
    }
    if flow.protocol == frame::ICMP {
        let id = source
            .or(destination)
            .map_or(flow.source_port, |(_, port)| port);
        next.source_port = id;
        next.destination_port = id;
    }
    next
}

/// A cable between two devices, either way round: the lower end first.
pub fn cable_key(a: End, b: End) -> (End, End) {
    if a <= b { (a, b) } else { (b, a) }
}

/// What a network is configured to be, and what `lifecycle.reset` goes back to.
#[derive(Debug, Clone)]
pub struct Baseline {
    /// The network's own address.
    pub network: Ipv4Addr,
    /// Its prefix length.
    pub prefix: u8,
    /// Whatever no rule matches.
    pub default_allow: bool,
    /// In order.
    pub rules: Vec<Rule>,
    /// Whether an answer to a conversation that was let through comes back
    /// without being judged. On unless turned off.
    pub answers: bool,
    /// Named sets of machines, for rules to name.
    pub groups: BTreeMap<String, Vec<String>>,
    /// The person's own services, beside the ones the page knows.
    pub services: BTreeMap<String, Service>,
    /// Name to address.
    pub dns: BTreeMap<String, Ipv4Addr>,
    /// The other records the router answers, by name: AAAA, CNAME, MX, TXT.
    /// (A records are `dns`, which the Server writes to as well.)
    pub records: Vec<(String, frame::Record)>,
    /// The domain DHCP hands out and names are searched in: `nas` and
    /// `nas.<domain>` are the same name.
    pub domain: String,
    /// Latency, loss and bandwidth.
    pub traffic: Traffic,
    /// Whether the router leads to the internet. Off unless turned on.
    pub internet: bool,
    /// Rules by name, in order: they come before the rules by address.
    pub names: Vec<NameRule>,
}

/// What a rule by name does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameAction {
    /// The router answers that the name does not exist.
    Hide,
    /// The name is this machine (a node's name) or address instead.
    Point(String),
    /// A connection is cut when it says the name: a TLS server name or an
    /// HTTP host, in its first bytes.
    Cut,
}

impl NameAction {
    /// `hide`, `point` or `cut`.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Hide => "hide",
            Self::Point(_) => "point",
            Self::Cut => "cut",
        }
    }
}

/// One rule by name. First match wins, as with the rules by address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameRule {
    /// `n1`, `n2`, ... in the order they were made.
    pub id: String,
    /// What it does.
    pub action: NameAction,
    /// `example.com`, or `*.example.com` for it and everything under it.
    pub name: String,
    /// Who asks: a node, `vlan:N`, `group:name`, a CIDR or `any`.
    pub source: String,
    /// How many times it decided, since the network started.
    pub uses: u64,
}

/// Whether `name` is what `pattern` says: the name itself, or with `*.`
/// in front, the name and everything that ends in it.
pub fn name_fits(pattern: &str, name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(base) => name == base || name.ends_with(&format!(".{base}")),
        None => name == pattern,
    }
}

impl Baseline {
    /// The router, DHCP server and DNS server: the first address.
    pub fn gateway(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) + 1)
    }

    /// Whether `address` is inside this network.
    pub fn contains(&self, address: Ipv4Addr) -> bool {
        within(address, self.network, self.prefix)
    }

    /// Whether `address` is out there: not this network, and not anything
    /// that would lead back into this computer or nowhere -- its loopback,
    /// link-local, multicast, broadcast or the unspecified address. A guest
    /// that could reach 127.0.0.1 through the router would reach the
    /// services of the computer BeatBox runs on.
    pub fn outside(&self, address: Ipv4Addr) -> bool {
        !self.contains(address)
            && !address.is_loopback()
            && !address.is_unspecified()
            && !address.is_link_local()
            && !address.is_multicast()
            && !address.is_broadcast()
            && address.octets()[0] != 0
    }

    /// The last address, which is everybody's.
    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) | !mask(self.prefix))
    }
}

fn mask(prefix: u8) -> u32 {
    u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
}

/// Whether `address` is in `network/prefix`.
pub fn within(address: Ipv4Addr, network: Ipv4Addr, prefix: u8) -> bool {
    u32::from(address) & mask(prefix) == u32::from(network) & mask(prefix)
}

#[derive(Debug, Clone)]
struct Seen {
    allowed: bool,
    generation: u64,
    packets: u64,
    last: Instant,
    /// The rule that decided, or `None`: the default, or an answer to a
    /// conversation that was allowed.
    rule: Option<String>,
    /// Whether it answered another conversation when it was first seen.
    answer: bool,
    /// Cut by its rule's limit, at this moment: it stays cut while the
    /// limit's while lasts.
    cut: Option<Instant>,
    /// Whether its answer came back, and whether that answer was a TCP
    /// reset: the machine was reached, and said no.
    replied: bool,
    reset: bool,
}

/// One name one node asked for, however many times.
#[derive(Debug, Clone, Default)]
struct Lookup {
    count: u64,
    answer: Option<Ipv4Addr>,
    /// The rule by name that answered it.
    rule: Option<String>,
}

/// How many different names are remembered. Past this, a new name is still
/// answered, only not written down.
const LOOKUPS_KEPT: usize = 256;

/// How many conversations are remembered. Past this, the ones quiet for a
/// minute are forgotten, and if that is not enough, all of them.
/// ponytail: forgetting everything drops established state for a moment;
/// an LRU if a lab ever holds this many live conversations.
const FLOWS_KEPT: usize = 4096;

/// What was counted since the last start or reset.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counters {
    /// Frames that arrived.
    pub received: u64,
    /// Frames sent on to a node.
    pub delivered: u64,
    /// Refused by a firewall rule.
    pub blocked: u64,
    /// Lost on purpose (`loss_percent`), or queued past a second.
    pub lost: u64,
    /// Not ARP or IPv4, from an address that is not the sender's, or to a
    /// node that is not there.
    pub refused: u64,
    /// For somewhere this network has no route to.
    pub unroutable: u64,
}

/// A network, while it is one.
#[derive(Debug, Clone)]
pub struct Net {
    /// What it is configured to be.
    pub baseline: Baseline,
    /// What its rules, names and traffic are now.
    pub live: Baseline,
    /// What is cabled to it.
    pub nodes: Vec<Node>,
    /// What happened.
    pub counters: Counters,
    next_rule: u64,
    flows: HashMap<Flow, Seen>,
    /// Every name each node asked for: (node, name).
    lookups: BTreeMap<(String, String), Lookup>,
    /// Bumped whenever a rule changes, so nothing that was allowed under the
    /// old rules counts as established under the new ones.
    generation: u64,
    /// What guests sent out through the router since the uplink last took it.
    leaving: Vec<Leaving>,
    /// The devices and cables the frames travel through.
    pub topology: Topology,
    /// Each router's networks and routes, by device name. The main router's
    /// VLAN 1 network is the network's own range (`live`), not in here.
    /// Kept on reset, like the topology: they are what the network is built of.
    pub routers: BTreeMap<String, RouterConf>,
    /// Nodes that are light machines (`crate::light`), by name, and what
    /// each is. Kept on reset, like the topology.
    pub lights: BTreeMap<String, crate::light::LightConf>,
    /// What each VLAN is called, by number: a label for people, nothing else.
    pub vlan_names: BTreeMap<u16, String>,
    /// Machines given another address that have not taken it yet, and the
    /// address they still speak as: they may use it for ARP, and only to
    /// reach the router, until they renew (`readdress`, `dhcp`).
    moved_from: BTreeMap<String, Ipv4Addr>,
    /// What a cable between two devices does on top of the network's own
    /// traffic (`Baseline::traffic`), by `cable_key`.
    pub link_traffic: BTreeMap<(End, End), Traffic>,
    /// What a machine's own cable does on top of it, by node name.
    pub node_traffic: BTreeMap<String, Traffic>,
    /// Frames across each cable between devices since the start.
    pub link_frames: BTreeMap<(End, End), u64>,
    /// Frames each machine sent and was sent since the start.
    pub node_frames: BTreeMap<String, (u64, u64)>,
    /// What the rules that log decided, oldest first.
    /// Each limited rule's count in its current while, by rule id and, for
    /// a limit counted per machine, the machine's address.
    pace: HashMap<(String, Option<IpAddr>), (Instant, u32)>,
    /// The way of the frame being handled now, when it is an IPv4 one.
    trail: Option<Trail>,
    /// Each conversation's way (`Trail`), kept beside `flows`.
    paths: HashMap<Flow, Trail>,
    /// NAT, as the outside sees it: a hidden machine's conversation by the
    /// router's address and port it left as, to the machine's own.
    nat: HashMap<NatKey, (Ipv4Addr, u16)>,
    /// The same, by the machine's own address and port.
    nat_out: HashMap<NatKey, (Ipv4Addr, u16)>,
    /// A forwarded conversation by the machine's end, to the router's
    /// address and port the answer has to come back from.
    unforward: HashMap<NatKey, (Ipv4Addr, u16)>,
    /// Captures, running and kept, oldest first (`crate::capture`).
    pub captures: Vec<crate::capture::Capture>,
    /// The IPv6 addresses machines have sent from, and whose they are: a
    /// system picks its own (SLAAC, privacy addresses), so they are learned.
    pub addresses6: BTreeMap<Ipv6Addr, String>,
    /// What each frame in the last `out` crossed, in the same order: what
    /// the shaper holds it to (`take_costs`).
    costs: Vec<Traffic>,
    /// Machines whose address stays theirs when the addresses handed out
    /// change: a virtual machine, the Server's, one reserved by hand.
    pub reserved: BTreeSet<String>,
    /// When each machine was last given its address (milliseconds since
    /// the epoch), since the start.
    pub leases: BTreeMap<String, u64>,
    /// Names in `live.dns` another sandbox put there, and which: they go
    /// with it, and are not written down as this network's own.
    pub published: BTreeMap<String, String>,
}

/// One of a router's networks: its range, and the router's own address in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    /// Network address and prefix.
    pub range: (Ipv4Addr, u8),
    /// The router's address there: the first, unless another router's is.
    pub address: Ipv4Addr,
}

/// A route a router was told: what is inside `to` goes to `via`, an address
/// on one of its own networks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// Where it leads; `0.0.0.0/0` is everywhere else.
    pub to: (Ipv4Addr, u8),
    /// The next router on the way.
    pub via: Ipv4Addr,
}

/// What a router is besides a device: its networks by VLAN, and its routes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouterConf {
    /// By VLAN.
    pub networks: BTreeMap<u16, Side>,
    /// Longest prefix first wins.
    pub routes: Vec<Route>,
    /// The VLANs whose network it hides: what leaves them through another
    /// of its networks leaves as the router, and nothing comes in that they
    /// did not start or a forward does not let in.
    pub hidden: BTreeSet<u16>,
    /// Ports on the router that lead to a machine behind it.
    pub forwards: Vec<Forward>,
    /// The VLANs whose network speaks IPv6 too: the router advertises its
    /// /64 there (`prefix6`), and carries IPv6 to and from it.
    pub ipv6: BTreeSet<u16>,
    /// How it hands out addresses in each VLAN's network, where that is not
    /// [`Dhcp::default`].
    pub dhcp: BTreeMap<u16, Dhcp>,
}

/// How a router hands out addresses in one of its networks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dhcp {
    /// Whether it answers at all: off, every machine there needs its
    /// address written in it.
    pub on: bool,
    /// The first address it gives, counted from the network's own (10 is .10).
    pub first: u32,
    /// The last, the same way; to the end of the range when none.
    pub last: Option<u32>,
    /// Seconds an address is held before it is asked for again.
    pub lease: u32,
    /// Who it says answers names; the router when none.
    pub dns: Option<Ipv4Addr>,
}

impl Default for Dhcp {
    fn default() -> Self {
        Self {
            on: true,
            first: 10,
            last: None,
            lease: 600,
            dns: None,
        }
    }
}

/// The shortest and longest lease a network hands out: a minute, a day.
pub const LEASES: (u32, u32) = (60, 86_400);

/// The /64 a network speaks IPv6 in, read off its IPv4 range so it can be
/// said aloud: 10.70.1.0/24 is fd00:10:70:1::/64. Each byte is written as
/// its decimal digits, which are hex digits too.
pub fn prefix6((network, _): (Ipv4Addr, u8)) -> Ipv6Addr {
    let o = network.octets();
    let group = |b: u8| u16::from_str_radix(&b.to_string(), 16).unwrap_or(0);
    Ipv6Addr::new(0xfd00, group(o[0]), group(o[1]), group(o[2]), 0, 0, 0, 0)
}

/// The router's own IPv6 address in a network: `::1` of its /64.
pub fn gateway6(range: (Ipv4Addr, u8)) -> Ipv6Addr {
    let mut segments = prefix6(range).segments();
    segments[7] = 1;
    Ipv6Addr::from(segments)
}

/// Whether `address` is in the /64 `prefix`.
pub fn in64(address: Ipv6Addr, prefix: Ipv6Addr) -> bool {
    address.segments()[..4] == prefix.segments()[..4]
}

/// An IPv4 address in the network a `prefix6` address came from: how a
/// router finds its way to an IPv6 network, by the routes it has for IPv4.
fn v4_of6(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = address.segments();
    if s[0] != 0xfd00 {
        return None;
    }
    let byte = |g: u16| format!("{g:x}").parse::<u8>().ok();
    Some(Ipv4Addr::new(byte(s[1])?, byte(s[2])?, byte(s[3])?, 1))
}

/// Whether `address` is link-local (fe80::/10).
fn link_local6(address: Ipv6Addr) -> bool {
    address.segments()[0] & 0xffc0 == 0xfe80
}

/// How often each router advertises its IPv6 networks unasked.
#[cfg(unix)]
const ANNOUNCE_EVERY: Duration = Duration::from_secs(30);

/// How many IPv6 addresses one machine is remembered by: a system makes a
/// few (stable, temporary, link-local), and a new one now and then.
const ADDRESSES6_KEPT: usize = 8;

/// A range's first address: the router's in it.
pub fn gateway_of((network, _): (Ipv4Addr, u8)) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(network) + 1)
}

/// Whether `address` may be a machine's in `range`: inside it, and not its
/// own address, the router's or the broadcast.
pub fn host_in(address: Ipv4Addr, range: (Ipv4Addr, u8)) -> bool {
    let (network, prefix) = range;
    within(address, network, prefix)
        && address != network
        && address != gateway_of(range)
        && address != Ipv4Addr::from(u32::from(network) | !mask(prefix))
}

/// Whether two ranges share any address.
pub fn overlap(a: (Ipv4Addr, u8), b: (Ipv4Addr, u8)) -> bool {
    within(a.0, b.0, b.1) || within(b.0, a.0, a.1)
}

/// A frame to send, and to which node (by index).
pub type Out = (usize, Vec<u8>);

/// A frame inside the network: the port it enters, its VLAN, the frame
/// without a tag, and what the cables it crossed so far do to it.
type Inside = (End, u16, Vec<u8>, Traffic);

/// A packet a guest sent out through the router, for the router's uplink to
/// carry (`crate::router`). The flow is the guest's own: its address and
/// port, and where it asked to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaving {
    /// As the guest sent it.
    pub flow: Flow,
    /// The whole IPv4 packet, header and all.
    pub packet: Vec<u8>,
    /// A name the network does not know, asked of the gateway: the uplink
    /// asks this computer's resolver instead of `flow.destination`, and the
    /// answer comes back from the gateway.
    pub resolve: bool,
}

impl Leaving {
    /// What follows the IPv4 header.
    fn transport(&self) -> &[u8] {
        let header = usize::from(self.packet.first().map_or(5, |b| b & 0x0f)) * 4;
        self.packet.get(header..).unwrap_or_default()
    }

    /// For UDP: the data, without its header.
    pub fn data(&self) -> &[u8] {
        self.transport().get(8..).unwrap_or_default()
    }

    /// For TCP: its flags (SYN is 0x02, ACK 0x10, RST 0x04, FIN 0x01).
    pub fn tcp_flags(&self) -> u8 {
        self.transport().get(13).copied().unwrap_or(0)
    }
}

/// An event to emit once the lock is let go.
pub type Said = (&'static str, Data);

impl Net {
    /// A network as configured, with nothing cabled to it.
    pub fn new(baseline: Baseline) -> Self {
        Self::restored(baseline.clone(), baseline)
    }

    /// A network configured as `baseline`, whose rules, names and traffic
    /// are `live` now -- one brought back from what it wrote down.
    pub fn restored(baseline: Baseline, live: Baseline) -> Self {
        let next_rule = live
            .rules
            .iter()
            .filter_map(|rule| rule.id.strip_prefix('r')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        Self {
            live,
            baseline,
            nodes: Vec::new(),
            counters: Counters::default(),
            next_rule,
            flows: HashMap::new(),
            lookups: BTreeMap::new(),
            generation: 0,
            leaving: Vec::new(),
            topology: Topology::default(),
            routers: BTreeMap::new(),
            lights: BTreeMap::new(),
            vlan_names: BTreeMap::new(),
            moved_from: BTreeMap::new(),
            link_traffic: BTreeMap::new(),
            node_traffic: BTreeMap::new(),
            link_frames: BTreeMap::new(),
            node_frames: BTreeMap::new(),
            pace: HashMap::new(),
            trail: None,
            paths: HashMap::new(),
            nat: HashMap::new(),
            nat_out: HashMap::new(),
            unforward: HashMap::new(),
            captures: Vec::new(),
            addresses6: BTreeMap::new(),
            costs: Vec::new(),
            reserved: BTreeSet::new(),
            leases: BTreeMap::new(),
            published: BTreeMap::new(),
        }
    }

    /// Back to how it was configured: rules, names and traffic. What is
    /// cabled stays cabled -- the guests on the other end still hold their
    /// cables, and unplugging them is `network.detach`.
    pub fn reset(&mut self) {
        let nodes = std::mem::take(&mut self.nodes);
        let topology = std::mem::take(&mut self.topology);
        let routers = std::mem::take(&mut self.routers);
        let lights = std::mem::take(&mut self.lights);
        let vlan_names = std::mem::take(&mut self.vlan_names);
        let link_traffic = std::mem::take(&mut self.link_traffic);
        let node_traffic = std::mem::take(&mut self.node_traffic);
        let reserved = std::mem::take(&mut self.reserved);
        *self = Self::new(self.baseline.clone());
        self.reserved = reserved;
        self.nodes = nodes;
        self.topology = topology;
        self.routers = routers;
        self.lights = lights;
        self.vlan_names = vlan_names;
        self.link_traffic = link_traffic;
        self.node_traffic = node_traffic;
    }

    /// Add a rule at the end; its id.
    pub fn add_rule(&mut self, mut rule: Rule) -> String {
        self.next_rule += 1;
        rule.id = format!("r{}", self.next_rule);
        let id = rule.id.clone();
        self.live.rules.push(rule);
        self.generation += 1;
        id
    }

    /// Put rule `id` at `index` (0 is first, past the end is last);
    /// whether there was one.
    pub fn move_rule(&mut self, id: &str, index: usize) -> bool {
        let Some(at) = self.live.rules.iter().position(|rule| rule.id == id) else {
            return false;
        };
        let rule = self.live.rules.remove(at);
        let index = index.min(self.live.rules.len());
        self.live.rules.insert(index, rule);
        self.generation += 1;
        true
    }

    /// Whether answers to conversations let through come back unjudged.
    pub fn set_answers(&mut self, on: bool) {
        self.live.answers = on;
        self.generation += 1;
    }

    /// Group `name` is these machines; none takes it away. A group a rule
    /// names stays: the rule would quietly fit nothing.
    pub fn set_group(
        &mut self,
        name: &str,
        members: Vec<String>,
    ) -> Result<(), beatbox_reasons::Reason> {
        let end = format!("group:{name}");
        if members.is_empty() {
            if let Some(rule) = self
                .live
                .rules
                .iter()
                .position(|rule| rule.source == end || rule.destination == end)
            {
                return Err(beatbox_reasons::reason!(
                    "network.group_in_use",
                    group = name,
                    n = rule + 1
                ));
            }
            self.live.groups.remove(name);
        } else {
            self.live.groups.insert(name.to_owned(), members);
        }
        self.generation += 1;
        Ok(())
    }

    /// What happens to a connection no rule fits: let through, or stopped.
    pub fn set_default(&mut self, allow: bool) {
        self.live.default_allow = allow;
        self.generation += 1;
    }

    /// The addresses NAT changed, when what it hides or lets in changed:
    /// the conversations are still told, and are judged again.
    fn forget_nat(&mut self) {
        self.nat.clear();
        self.nat_out.clear();
        self.unforward.clear();
        self.generation += 1;
    }

    /// Conversations and names remembered under addresses that no longer
    /// mean what they did; the next packet starts them again.
    pub fn forget_conversations(&mut self) {
        self.flows.clear();
        self.lookups.clear();
        self.paths.clear();
        self.nat.clear();
        self.nat_out.clear();
        self.unforward.clear();
        self.pace.clear();
        self.generation += 1;
    }

    /// Whether the router leads to the internet.
    pub fn set_internet(&mut self, on: bool) {
        self.live.internet = on;
        self.generation += 1;
    }

    /// What each frame of the last `handle` or `arrive` crossed, in the
    /// order of its `out`: the network's own traffic, the cables between
    /// devices on its way, and the machines' cables at both ends.
    pub fn take_costs(&mut self) -> Vec<Traffic> {
        std::mem::take(&mut self.costs)
    }

    /// Counters and per-cable counts back to nothing: a start.
    pub fn clear_counts(&mut self) {
        self.counters = Counters::default();
        self.link_frames.clear();
        self.node_frames.clear();
    }

    /// What guests sent out through the router, for the uplink to carry.
    pub fn take_leaving(&mut self) -> Vec<Leaving> {
        std::mem::take(&mut self.leaving)
    }

    /// Take a rule away; whether there was one.
    pub fn remove_rule(&mut self, id: &str) -> bool {
        let before = self.live.rules.len();
        self.live.rules.retain(|rule| rule.id != id);
        self.generation += 1;
        self.live.rules.len() != before
    }

    /// The node by name.
    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.name == name)
    }

    /// The router devices, the main one first.
    pub fn router_devices(&self) -> Vec<usize> {
        (0..self.topology.devices.len())
            .filter(|d| self.topology.devices[*d].kind == Kind::Router)
            .collect()
    }

    fn conf(&self, device: usize) -> Option<&RouterConf> {
        self.routers.get(&self.topology.devices.get(device)?.name)
    }

    /// Router `device`'s networks by VLAN; the main one's VLAN 1 is the
    /// network's own range.
    pub fn sides(&self, device: usize) -> Vec<(u16, Side)> {
        let mut all = Vec::new();
        if Some(device) == self.topology.router() {
            all.push((
                DEFAULT_VLAN,
                Side {
                    range: (self.live.network, self.live.prefix),
                    address: self.live.gateway(),
                },
            ));
        }
        if let Some(conf) = self.conf(device) {
            all.extend(conf.networks.iter().map(|(vlan, side)| (*vlan, *side)));
        }
        all
    }

    /// Router `device`'s network in `vlan`.
    pub fn side(&self, device: usize, vlan: u16) -> Option<Side> {
        self.sides(device)
            .into_iter()
            .find(|(v, _)| *v == vlan)
            .map(|(_, side)| side)
    }

    /// Every router's every network: device, VLAN, network.
    fn every_side(&self) -> Vec<(usize, u16, Side)> {
        self.router_devices()
            .into_iter()
            .flat_map(|device| {
                self.sides(device)
                    .into_iter()
                    .map(move |(vlan, side)| (device, vlan, side))
            })
            .collect()
    }

    /// The main router's networks besides VLAN 1's, by VLAN.
    pub fn interfaces(&self) -> BTreeMap<u16, (Ipv4Addr, u8)> {
        self.topology
            .router()
            .and_then(|main| self.conf(main))
            .map(|conf| conf.networks.iter().map(|(v, s)| (*v, s.range)).collect())
            .unwrap_or_default()
    }

    /// A router's own MAC: the main one's is `GATEWAY_MAC`, the next ones
    /// count down from it.
    pub fn router_mac(&self, device: usize) -> Mac {
        let order = self
            .router_devices()
            .iter()
            .position(|d| *d == device)
            .unwrap_or(0);
        let mut mac = GATEWAY_MAC;
        mac[5] = mac[5].wrapping_sub(order as u8);
        mac
    }

    fn router_by_mac(&self, mac: Mac) -> Option<usize> {
        self.router_devices()
            .into_iter()
            .find(|d| self.router_mac(*d) == mac)
    }

    /// The router whose address `address` is.
    fn owner(&self, address: Ipv4Addr) -> Option<usize> {
        self.every_side()
            .into_iter()
            .find(|(_, _, side)| side.address == address)
            .map(|(device, _, _)| device)
    }

    /// The router that serves node `node` -- hands it its address, is its
    /// gateway: the first that has a network in the VLAN the node reaches
    /// it in. With it, that VLAN and network.
    pub fn serving(&self, node: usize) -> Option<(usize, u16, Side)> {
        let at = self.nodes[node].at;
        self.router_devices().into_iter().find_map(|router| {
            let vlan = self.topology.vlan_toward(at, router)?;
            Some((router, vlan, self.side(router, vlan)?))
        })
    }

    /// The range of the network node `node` is in.
    pub fn subnet_of(&self, node: usize) -> Option<(Ipv4Addr, u8)> {
        self.serving(node).map(|(_, _, side)| side.range)
    }

    /// The main router's range in `vlan`.
    pub fn subnet(&self, vlan: u16) -> Option<(Ipv4Addr, u8)> {
        self.side(self.topology.router()?, vlan)
            .map(|side| side.range)
    }

    /// Whether `address` is one of a router's own.
    pub fn is_router(&self, address: IpAddr) -> bool {
        match address {
            IpAddr::V4(address) => self.owner(address).is_some(),
            IpAddr::V6(address) => self.owner6(address).is_some(),
        }
    }

    /// Router `device`'s networks that speak IPv6: VLAN, network, /64.
    pub fn sides6(&self, device: usize) -> Vec<(u16, Side, Ipv6Addr)> {
        let Some(conf) = self.conf(device) else {
            return Vec::new();
        };
        self.sides(device)
            .into_iter()
            .filter(|(vlan, _)| conf.ipv6.contains(vlan))
            .map(|(vlan, side)| (vlan, side, prefix6(side.range)))
            .collect()
    }

    /// Whether `vlan`'s network on router `router` speaks IPv6.
    pub fn speaks6(&self, router: usize, vlan: u16) -> bool {
        self.conf(router)
            .is_some_and(|conf| conf.ipv6.contains(&vlan))
    }

    /// The router whose IPv6 address `address` is: `::1` of one of its
    /// networks that speak IPv6, or its link-local address.
    fn owner6(&self, address: Ipv6Addr) -> Option<usize> {
        self.router_devices().into_iter().find(|device| {
            frame::link_local(self.router_mac(*device)) == address
                || self
                    .sides6(*device)
                    .iter()
                    .any(|(_, side, _)| gateway6(side.range) == address)
        })
    }

    /// Whether node `node` holds `address`: its IPv4 address, or an IPv6 one
    /// it sent from or makes from its MAC.
    fn holds(&self, node: &Node, address: IpAddr) -> bool {
        match address {
            IpAddr::V4(address) => node.address == address,
            IpAddr::V6(address) => {
                let mut prefix = address.segments();
                prefix[4..].fill(0);
                self.addresses6.get(&address) == Some(&node.name)
                    || frame::link_local(node.mac) == address
                    || frame::eui64(Ipv6Addr::from(prefix), node.mac) == address
            }
        }
    }

    /// The node that holds `address`.
    pub fn holder(&self, address: IpAddr) -> Option<usize> {
        self.nodes.iter().position(|node| self.holds(node, address))
    }

    /// A node's IPv6 addresses, its link-local one last: those it sent
    /// from, and for a light machine the one it makes in each /64 it is in.
    pub fn addresses6_of(&self, node: usize) -> Vec<Ipv6Addr> {
        let me = &self.nodes[node];
        let mut all: Vec<Ipv6Addr> = self
            .addresses6
            .iter()
            .filter(|(address, name)| **name == me.name && !link_local6(**address))
            .map(|(address, _)| *address)
            .collect();
        if self.lights.contains_key(&me.name)
            && let Some((router, vlan, side)) = self.serving(node)
            && self.speaks6(router, vlan)
        {
            let mine = frame::eui64(prefix6(side.range), me.mac);
            if !all.contains(&mine) {
                all.insert(0, mine);
            }
        }
        all
    }

    /// Remember that node `node` sent from `address`, if it may: a
    /// link-local address, or one in a /64 of the VLAN it is in that no
    /// other machine holds. A machine keeps its newest few.
    fn learn6(&mut self, node: usize, address: Ipv6Addr) {
        let name = self.nodes[node].name.clone();
        if address.is_unspecified() || self.addresses6.get(&address) == Some(&name) {
            return;
        }
        let ours = link_local6(address)
            || self.serving(node).is_some_and(|(router, vlan, side)| {
                self.speaks6(router, vlan) && in64(address, prefix6(side.range))
            });
        if !ours || self.addresses6.contains_key(&address) {
            return;
        }
        let mine: Vec<Ipv6Addr> = self
            .addresses6
            .iter()
            .filter(|(_, n)| **n == name)
            .map(|(a, _)| *a)
            .collect();
        if mine.len() >= ADDRESSES6_KEPT {
            self.addresses6.remove(&mine[0]);
        }
        self.addresses6.insert(address, name);
    }

    /// Whether `address` is in any router's networks.
    fn inside(&self, address: Ipv4Addr) -> bool {
        self.every_side()
            .iter()
            .any(|(_, _, side)| within(address, side.range.0, side.range.1))
    }

    /// Out there: in none of the routers' networks, and nothing that leads
    /// back into this computer (`Baseline::outside`).
    pub fn outside(&self, address: Ipv4Addr) -> bool {
        self.live.outside(address) && !self.inside(address)
    }

    /// The main router's network in VLAN `vlan`, with its first address
    /// for the router (`set_network`).
    pub fn set_interface(
        &mut self,
        vlan: u16,
        range: Option<(Ipv4Addr, u8)>,
    ) -> Result<(), beatbox_reasons::Reason> {
        let main = self
            .topology
            .router()
            .ok_or_else(|| beatbox_reasons::reason!("network.no_such_device"))?;
        let side = range.map(|range| Side {
            range,
            address: gateway_of(range),
        });
        self.set_network(main, vlan, side)
    }

    /// Give router `device` a network in VLAN `vlan`, change it, or take it
    /// away (`None`). The main router's VLAN 1 is the main range, changed by
    /// renumbering. No two networks share an address, except one that two
    /// routers are both on (a link between them), each at its own address.
    pub fn set_network(
        &mut self,
        device: usize,
        vlan: u16,
        side: Option<Side>,
    ) -> Result<(), beatbox_reasons::Reason> {
        use beatbox_reasons::reason;
        if Some(device) == self.topology.router() && vlan == DEFAULT_VLAN {
            return Err(reason!("network.main_vlan"));
        }
        if !crate::topology::VLANS.contains(&vlan) {
            return Err(reason!("network.bad_vlan", vlan = vlan));
        }
        let Some(name) = self
            .topology
            .devices
            .get(device)
            .filter(|d| d.kind == Kind::Router)
            .map(|d| d.name.clone())
        else {
            return Err(reason!("network.no_such_device"));
        };
        let Some(side) = side else {
            if let Some(conf) = self.routers.get_mut(&name) {
                conf.networks.remove(&vlan);
            }
            self.generation += 1;
            return Ok(());
        };
        let cidr = format!("{}/{}", side.range.0, side.range.1);
        let broadcast = Ipv4Addr::from(u32::from(side.range.0) | !mask(side.range.1));
        if !within(side.address, side.range.0, side.range.1)
            || side.address == side.range.0
            || side.address == broadcast
        {
            return Err(reason!(
                "network.address_outside",
                address = side.address,
                cidr = cidr
            ));
        }
        for (other_device, other_vlan, other) in self.every_side() {
            if other_device == device && other_vlan == vlan {
                continue;
            }
            if !overlap(side.range, other.range) {
                continue;
            }
            if side.range != other.range || other_device == device {
                return Err(reason!(
                    "network.overlap",
                    cidr = cidr,
                    other = format!("{}/{}", other.range.0, other.range.1)
                ));
            }
            if other.address == side.address {
                return Err(reason!("network.address_taken", address = side.address));
            }
        }
        if self.nodes.iter().any(|n| n.address == side.address) {
            return Err(reason!("network.address_taken", address = side.address));
        }
        self.routers
            .entry(name)
            .or_default()
            .networks
            .insert(vlan, side);
        self.generation += 1;
        Ok(())
    }

    /// Tell router `device` a route; one to the same place is replaced. The
    /// next hop has to be on one of its networks, and not itself.
    pub fn add_route(
        &mut self,
        device: usize,
        route: Route,
    ) -> Result<(), beatbox_reasons::Reason> {
        let on_a_network = self.sides(device).iter().any(|(_, side)| {
            within(route.via, side.range.0, side.range.1) && side.address != route.via
        });
        if !on_a_network {
            return Err(beatbox_reasons::reason!(
                "network.route_via",
                via = route.via,
                router = self.topology.devices[device].name
            ));
        }
        let name = self.topology.devices[device].name.clone();
        let conf = self.routers.entry(name).or_default();
        conf.routes.retain(|r| r.to != route.to);
        conf.routes.push(route);
        self.generation += 1;
        Ok(())
    }

    /// Forget router `device`'s route to `to`; whether it had one.
    pub fn remove_route(&mut self, device: usize, to: (Ipv4Addr, u8)) -> bool {
        let name = self.topology.devices[device].name.clone();
        let Some(conf) = self.routers.get_mut(&name) else {
            return false;
        };
        let before = conf.routes.len();
        conf.routes.retain(|r| r.to != to);
        self.generation += 1;
        before != conf.routes.len()
    }

    /// Where router `router` sends a packet for `destination` next: there
    /// itself when it is on one of its networks, or the route that names
    /// the longest part of it.
    fn next_hop(&self, router: usize, destination: Ipv4Addr) -> Option<Ipv4Addr> {
        if self
            .sides(router)
            .iter()
            .any(|(_, side)| within(destination, side.range.0, side.range.1))
        {
            return Some(destination);
        }
        self.conf(router)?
            .routes
            .iter()
            .filter(|r| within(destination, r.to.0, r.to.1))
            .max_by_key(|r| r.to.1)
            .map(|r| r.via)
    }

    /// How router `router` reaches `address` on one of its networks: the
    /// port, the VLAN, and the MAC to send to -- a machine's, or another
    /// router's on a network they share.
    fn hop_to(&self, router: usize, address: Ipv4Addr) -> Option<(u16, u16, Mac)> {
        if let Some(node) = self.nodes.iter().position(|n| n.address == address) {
            let at = self.nodes[node].at;
            let vlan = self.topology.vlan_toward(at, router)?;
            let side = self.side(router, vlan)?;
            if !within(address, side.range.0, side.range.1) {
                return None;
            }
            return Some((
                self.topology.toward_end(router, at)?,
                vlan,
                self.nodes[node].mac,
            ));
        }
        let (other, vlan, side) = self
            .every_side()
            .into_iter()
            .find(|(device, _, side)| *device != router && side.address == address)?;
        (self.side(router, vlan)?.range == side.range).then_some(())?;
        Some((
            self.topology.toward(router, other)?,
            vlan,
            self.router_mac(other),
        ))
    }

    /// Every machine's address inside the network of the VLAN it reaches
    /// the router in. One that is not -- its port went to another VLAN, the
    /// VLAN's range changed -- takes the first free address there, and the
    /// names that pointed at the old one follow; the machine hears it when
    /// it next renews (`frame::dhcp_answer`). What moved: node, from, to.
    /// One handed out by the router, not reserved, outside the addresses it
    /// hands out now, moves into them the same way.
    pub fn readdress(&mut self) -> Vec<(String, Ipv4Addr, Ipv4Addr)> {
        let mut moved = Vec::new();
        for node in 0..self.nodes.len() {
            let Some(range) = self.subnet_of(node) else {
                continue;
            };
            let old = self.nodes[node].address;
            let (first, last) = self.pool(range);
            let pooled = (first..=last).contains(&u32::from(old));
            if host_in(old, range) && !self.is_router(old.into()) && (pooled || self.keeps(node)) {
                continue;
            }
            let Some(new) = self.free_in(range) else {
                continue;
            };
            self.nodes[node].address = new;
            self.moved_from
                .entry(self.nodes[node].name.clone())
                .or_insert(old);
            for address in self.live.dns.values_mut() {
                if *address == old {
                    *address = new;
                }
            }
            moved.push((self.nodes[node].name.clone(), old, new));
        }
        if !moved.is_empty() {
            self.forget_conversations();
        }
        moved
    }

    /// Give node `name` the address `to` in place of `from`: it may still
    /// reach the router as `from` until it renews (`moved_from`).
    pub fn move_node(&mut self, name: &str, from: Ipv4Addr, to: Ipv4Addr) {
        if let Some(node) = self.nodes.iter_mut().find(|n| n.name == name) {
            node.address = to;
            self.moved_from.entry(name.to_owned()).or_insert(from);
        }
    }

    fn named(&self, address: IpAddr) -> Option<&str> {
        if self.is_router(address) {
            return Some("gateway");
        }
        self.holder(address)
            .map(|node| self.nodes[node].name.as_str())
    }

    /// The first address of the main range from .10 up that nobody holds.
    pub fn free_address(&self) -> Option<Ipv4Addr> {
        self.free_in((self.live.network, self.live.prefix))
    }

    /// The first address the router hands out in `range` that nobody holds.
    fn free_in(&self, range: (Ipv4Addr, u8)) -> Option<Ipv4Addr> {
        let (first, last) = self.pool(range);
        (first..=last).map(Ipv4Addr::from).find(|a| {
            self.nodes.iter().all(|node| node.address != *a) && !self.is_router((*a).into())
        })
    }

    /// How router `router` hands out addresses in `vlan`'s network.
    pub fn dhcp_of(&self, router: usize, vlan: u16) -> Dhcp {
        self.conf(router)
            .and_then(|conf| conf.dhcp.get(&vlan))
            .copied()
            .unwrap_or_default()
    }

    /// How the network `range` is hands out addresses: its router's way.
    fn dhcp_in(&self, range: (Ipv4Addr, u8)) -> Dhcp {
        self.every_side()
            .into_iter()
            .find(|(_, _, side)| side.range == range)
            .map_or_else(Dhcp::default, |(device, vlan, _)| {
                self.dhcp_of(device, vlan)
            })
    }

    /// The first and last address handed out in `range`, as numbers; never
    /// the network's own or its broadcast.
    pub fn pool(&self, range: (Ipv4Addr, u8)) -> (u32, u32) {
        let dhcp = self.dhcp_in(range);
        let base = u32::from(range.0);
        let end = (base | !mask(range.1)) - 1;
        let first = base.saturating_add(dhcp.first).clamp(base + 1, end);
        let last = dhcp
            .last
            .map_or(end, |last| base.saturating_add(last).min(end));
        (first, last.max(first))
    }

    /// Whether node `node` keeps its address whatever is handed out: it is
    /// reserved, a light machine with its address written in it, or in a
    /// network that hands out none.
    fn keeps(&self, node: usize) -> bool {
        let name = &self.nodes[node].name;
        self.reserved.contains(name)
            || self.lights.get(name).is_some_and(|light| !light.dhcp)
            || self
                .subnet_of(node)
                .is_some_and(|range| !self.dhcp_in(range).on)
    }

    /// Set how router `router` hands out addresses in `vlan`'s network. The
    /// machines that take theirs from it and are outside it now move in
    /// (`readdress`).
    pub fn set_dhcp(
        &mut self,
        router: usize,
        vlan: u16,
        dhcp: Dhcp,
    ) -> Result<(), beatbox_reasons::Reason> {
        use beatbox_reasons::reason;
        let name = self.topology.devices[router].name.clone();
        let Some(side) = self.side(router, vlan) else {
            return Err(reason!(
                "network.nat_no_network",
                router = name,
                vlan = vlan
            ));
        };
        let size = !mask(side.range.1);
        let last = dhcp.last.unwrap_or(size - 1);
        if dhcp.first < 2 || last >= size || dhcp.first > last {
            let at = |offset: u32| Ipv4Addr::from(u32::from(side.range.0).wrapping_add(offset));
            return Err(reason!(
                "network.bad_pool",
                first = at(dhcp.first),
                last = at(last)
            ));
        }
        if !(LEASES.0..=LEASES.1).contains(&dhcp.lease) {
            return Err(reason!("network.bad_lease", seconds = dhcp.lease));
        }
        let conf = self.routers.entry(name).or_default();
        if dhcp == Dhcp::default() {
            conf.dhcp.remove(&vlan);
        } else {
            conf.dhcp.insert(vlan, dhcp);
        }
        Ok(())
    }

    /// Which node's cable `path` is.
    pub fn port_of(&self, path: &Path) -> Option<usize> {
        self.nodes.iter().position(|node| node.port == path)
    }

    /// What `name` resolves to: a record, a node (`name`, or `name.` anything
    /// under it), or the gateway.
    /// A CNAME is followed to what it names.
    pub fn resolve(&self, name: &str) -> Option<Ipv4Addr> {
        let name = self.canonical(name);
        for form in self.forms(&name) {
            if let Some(address) = self.live.dns.get(&form) {
                return Some(*address);
            }
        }
        let host = name.split('.').next().unwrap_or_default();
        if host == "gateway" {
            return Some(self.live.gateway());
        }
        self.node(host).map(|node| node.address)
    }

    /// What the router answers about `name` for a question of `kind`, each
    /// with the name it is about: the CNAMEs on the way first, then what
    /// the name they end at has of that kind.
    pub fn answers(&self, name: &str, kind: u16) -> Vec<(String, frame::Record)> {
        let mut said = Vec::new();
        let mut at = name.trim_end_matches('.').to_ascii_lowercase();
        if kind != 5 {
            for _ in 0..8 {
                let Some(frame::Record::Cname(to)) = self.records(&at, 5).first().copied() else {
                    break;
                };
                said.push((at.clone(), frame::Record::Cname(to.clone())));
                at = to.clone();
            }
        }
        match kind {
            1 => said.extend(self.resolve(&at).map(|a| (at.clone(), frame::Record::A(a)))),
            28 => said.extend(
                self.resolve6(&at)
                    .map(|a| (at.clone(), frame::Record::Aaaa(a))),
            ),
            _ => said.extend(
                self.records(&at, kind)
                    .into_iter()
                    .map(|record| (at.clone(), record.clone())),
            ),
        }
        said
    }

    /// `name` as asked, and without or with the network's domain: `nas` and
    /// `nas.lab` are one name.
    fn forms(&self, name: &str) -> Vec<String> {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        let domain = &self.live.domain;
        let other = match name.strip_suffix(&format!(".{domain}")) {
            Some(bare) => bare.to_owned(),
            None => format!("{name}.{domain}"),
        };
        vec![name, other]
    }

    /// The records of `kind` the router holds for `name`, in any of its forms.
    pub fn records(&self, name: &str, kind: u16) -> Vec<&frame::Record> {
        let forms = self.forms(name);
        self.live
            .records
            .iter()
            .filter(|(owner, record)| record.kind() == kind && forms.contains(owner))
            .map(|(_, record)| record)
            .collect()
    }

    /// Where `name` leads past its CNAMEs: itself when it has none. Eight at
    /// most, so two that name each other end.
    pub fn canonical(&self, name: &str) -> String {
        let mut name = name.trim_end_matches('.').to_ascii_lowercase();
        for _ in 0..8 {
            match self.records(&name, 5).first() {
                Some(frame::Record::Cname(to)) => name = to.clone(),
                _ => break,
            }
        }
        name
    }

    /// Whether the router has anything at all for `name`: an address, or any
    /// record. An unknown name is asked of the internet when there is one.
    pub fn knows(&self, name: &str) -> bool {
        self.resolve(name).is_some() || {
            let forms = self.forms(name);
            self.live
                .records
                .iter()
                .any(|(owner, _)| forms.contains(owner))
        }
    }

    /// What `name` resolves to in IPv6: the machine's own address, when it
    /// has one, or the router's.
    pub fn resolve6(&self, name: &str) -> Option<Ipv6Addr> {
        if let Some(frame::Record::Aaaa(address)) = self.records(&self.canonical(name), 28).first()
        {
            return Some(*address);
        }
        let address = self.resolve(name)?;
        if let Some(router) = self.owner(address) {
            return self
                .sides6(router)
                .into_iter()
                .find(|(_, side, _)| side.address == address)
                .map(|(_, side, _)| gateway6(side.range));
        }
        let node = self.nodes.iter().position(|n| n.address == address)?;
        self.addresses6_of(node).into_iter().next()
    }

    fn matches(&self, spec: &str, address: IpAddr) -> bool {
        match spec {
            "" | "any" | "*" => true,
            "gateway" => self.is_router(address),
            // ponytail: no IPv6 leads outside yet.
            "internet" => matches!(address, IpAddr::V4(a) if self.outside(a)),
            _ => {
                // ponytail: a VLAN is the networks routers have in it; a
                // machine with a fixed address from elsewhere is not in it.
                if let Some(vlan) = spec.strip_prefix("vlan:") {
                    let vlan = vlan.parse::<u16>().ok();
                    return self.every_side().iter().any(|(device, v, side)| {
                        Some(*v) == vlan
                            && match address {
                                IpAddr::V4(a) => within(a, side.range.0, side.range.1),
                                IpAddr::V6(a) => {
                                    self.speaks6(*device, *v) && in64(a, prefix6(side.range))
                                }
                            }
                    });
                }
                if let Some(group) = spec.strip_prefix("group:") {
                    return self.live.groups.get(group).is_some_and(|members| {
                        members
                            .iter()
                            .any(|m| self.node(m).is_some_and(|n| self.holds(n, address)))
                    });
                }
                if let Some(node) = self.node(spec) {
                    return self.holds(node, address);
                }
                match (parse_cidr(spec), parse_cidr6(spec), address) {
                    (Some((network, prefix)), _, IpAddr::V4(a)) => within(a, network, prefix),
                    (_, Some((network, prefix)), IpAddr::V6(a)) => {
                        let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
                        u128::from(a) & mask == u128::from(network) & mask
                    }
                    _ => false,
                }
            }
        }
    }

    /// What the rules say, and which one (its id):
    /// `None`, the default. `icmp` is the ICMP type of an ICMP packet;
    /// `answering`, whether the packet answers a conversation already seen.
    fn verdict(&self, flow: Flow, icmp: Option<u8>, answering: bool) -> (Verdict, Option<String>) {
        let v6 = flow.is_v6();
        for rule in &self.live.rules {
            if rule.ethertype.is_none()
                && !(rule.only_new && answering)
                && rule.family.is_none_or(|family| (family == 6) == v6)
                && self.matches(&rule.source, flow.source)
                && self.matches(&rule.destination, flow.destination)
                && rule.protocol.is_none_or(|p| {
                    p == flow.protocol || (v6 && p == frame::ICMP && flow.protocol == frame::ICMPV6)
                })
                && rule
                    .icmp_type
                    .is_none_or(|kind| icmp == Some(if v6 { icmp6_of(kind) } else { kind }))
                && (matches!(flow.protocol, frame::ICMP | frame::ICMPV6)
                    || rule.ports.is_none_or(|(first, last)| {
                        (first..=last).contains(&flow.destination_port)
                    }))
            {
                return (rule.verdict, Some(rule.id.clone()));
            }
        }
        (self.default_verdict(), None)
    }

    fn default_verdict(&self) -> Verdict {
        if self.live.default_allow {
            Verdict::Allow
        } else {
            Verdict::Deny
        }
    }

    /// A frame that is not IP or ARP, from node `from` (or another router)
    /// to `to`: judged only by the rules that name its EtherType, else by
    /// the default. Not remembered, and never answered.
    fn judge_other(&self, ethertype: u16, from: Option<usize>, to: Mac) -> bool {
        // Nobody in particular: only `any` fits.
        let source = from.map_or(Ipv4Addr::BROADCAST, |f| self.nodes[f].address);
        let destination = self
            .nodes
            .iter()
            .find(|n| n.mac == to)
            .map_or(Ipv4Addr::BROADCAST, |n| n.address);
        self.live
            .rules
            .iter()
            .find(|rule| {
                rule.ethertype == Some(ethertype)
                    && self.matches(&rule.source, source.into())
                    && self.matches(&rule.destination, destination.into())
            })
            .map_or(self.default_verdict(), |rule| rule.verdict)
            == Verdict::Allow
    }

    /// Decide on one conversation's packet, remember it, and say so when the
    /// answer is news: a conversation that begins, or one that is refused.
    fn police(&mut self, flow: Flow, ip: &[u8], said: &mut Vec<Said>) -> Verdict {
        let icmp = icmp_type(ip);
        let reverse = self.flows.get(&flow.reverse());
        let answering = reverse.is_some();
        let answered = self.live.answers
            && reverse.is_some_and(|r| r.allowed && r.generation == self.generation);
        let (mut verdict, mut rule) = self.verdict(flow, icmp, answering);
        if verdict != Verdict::Allow && answered {
            verdict = Verdict::Allow;
            // Let through as an answer, not by the rule that would refuse it.
            rule = None;
        }
        // A connection a rule by name cut stays cut: what it still sends
        // (its reset, an answer to a keep-alive) is dropped, told by that rule.
        let cut_by = self
            .flows
            .get(&flow)
            .filter(|seen| !seen.allowed)
            .and_then(|seen| seen.rule.clone())
            .filter(|id| self.live.names.iter().any(|n| &n.id == id));
        if let Some(id) = cut_by {
            // A reset closes the other end: the server still holding the
            // connection the router cut. It passes, told as nothing new.
            if tcp_reset(ip) {
                return Verdict::Allow;
            }
            verdict = Verdict::Deny;
            rule = Some(id);
        }
        // A connection that says a name a rule cuts: refused there, by that
        // rule, whatever let it begin.
        if let Some(name) = said_name(ip)
            && let Some(i) = self.name_rule(&name, flow.source, true)
        {
            self.live.names[i].uses += 1;
            verdict = Verdict::Reject;
            rule = Some(self.live.names[i].id.clone());
        }
        // A rule's limit counts new conversations: a packet of one already
        // let through is not one more, and one already cut stays cut.
        let mut cut = None;
        let limit = rule.as_ref().and_then(|id| {
            self.live
                .rules
                .iter()
                .find(|r| &r.id == id)
                .and_then(|r| r.limit)
        });
        if let (Verdict::Allow, Some(limit), Some(id)) = (verdict, limit, &rule)
            && !answering
        {
            let per = Duration::from_millis(limit.per_ms);
            match self.flows.get(&flow) {
                Some(seen) if seen.cut.is_some_and(|at| at.elapsed() < per) => cut = seen.cut,
                Some(_) => {}
                None => {
                    if self.pace.len() >= FLOWS_KEPT {
                        self.pace
                            .retain(|_, (since, _)| since.elapsed() < Duration::from_secs(3600));
                    }
                    let key = (id.clone(), limit.each.then_some(flow.source));
                    let (since, count) = self.pace.entry(key).or_insert((Instant::now(), 0));
                    if since.elapsed() >= per {
                        *since = Instant::now();
                        *count = 0;
                    }
                    if *count >= limit.count {
                        cut = Some(Instant::now());
                    } else {
                        *count += 1;
                    }
                }
            }
            if cut.is_some() {
                verdict = limit.over;
            }
        }
        let allowed = verdict == Verdict::Allow;
        if let Some(trail) = &mut self.trail
            && trail.flow == Some(flow)
        {
            trail.judged = Some((verdict, rule.clone()));
            trail.limited = cut.is_some();
        }
        if self.flows.len() >= FLOWS_KEPT && !self.flows.contains_key(&flow) {
            self.flows
                .retain(|_, seen| seen.last.elapsed() < Duration::from_secs(60));
            if self.flows.len() >= FLOWS_KEPT {
                self.flows.clear();
            }
        }
        let generation = self.generation;
        let previous = self.flows.insert(
            flow,
            Seen {
                allowed,
                generation,
                packets: 0,
                last: Instant::now(),
                rule: rule.clone(),
                answer: answering,
                cut,
                replied: false,
                reset: false,
            },
        );
        let seen = self.flows.get_mut(&flow).expect("just put there");
        seen.packets = previous.as_ref().map_or(0, |p| p.packets) + 1;
        seen.answer = previous.as_ref().map_or(answering, |p| p.answer);
        seen.replied = previous.as_ref().is_some_and(|p| p.replied);
        seen.reset = previous.as_ref().is_some_and(|p| p.reset);
        if allowed
            && answering
            && let Some(request) = self.flows.get_mut(&flow.reverse())
        {
            request.replied = true;
            request.reset |= tcp_reset(ip);
        }
        let news = match &previous {
            None => !(allowed && answering),
            Some(before) => before.allowed != allowed,
        };
        if news {
            let mut data = self.describe(flow);
            data.insert("action".into(), verdict.name().into());
            data.insert(
                "rule".into(),
                rule.map_or(serde_json::Value::Null, Into::into),
            );
            data.insert("limited".into(), cut.is_some().into());
            said.push((
                if allowed {
                    "connection.created"
                } else {
                    "packet.blocked"
                },
                data,
            ));
        }
        verdict
    }

    /// "No" for whoever sent `ip`, as if from where it was going: port
    /// unreachable for TCP and UDP, so the program hears "refused", and
    /// "administratively prohibited" for anything else.
    fn refusal(&self, router: usize, sender: Mac, flow: Flow, ip: &[u8]) -> Vec<u8> {
        // A connection already open is cut with a reset, as a firewall that
        // cuts one does: an "unreachable" is not heard once it is open.
        if let Some(reset) = frame::tcp_reset(ip) {
            let ethertype = if flow.is_v6() {
                frame::ETHER_IPV6
            } else {
                frame::ETHER_IPV4
            };
            return frame::ethernet(sender, self.router_mac(router), ethertype, &reset);
        }
        if let (IpAddr::V6(from), IpAddr::V6(to)) = (flow.destination, flow.source) {
            let code = if matches!(flow.protocol, frame::TCP | frame::UDP) {
                4
            } else {
                1
            };
            let icmp = frame::icmpv6_error(frame::ICMPV6_UNREACHABLE, code, ip);
            let packet = frame::icmpv6_packet(from, to, 64, icmp);
            return frame::ethernet(sender, self.router_mac(router), frame::ETHER_IPV6, &packet);
        }
        let code = if matches!(flow.protocol, frame::TCP | frame::UDP) {
            3
        } else {
            13
        };
        let icmp = frame::icmp_error(frame::ICMP_UNREACHABLE, code, ip);
        let packet = frame::ipv4_packet(flow.destination4(), flow.source4(), frame::ICMP, &icmp);
        frame::ethernet(sender, self.router_mac(router), frame::ETHER_IPV4, &packet)
    }

    fn describe(&self, flow: Flow) -> Data {
        data!({
            "source": flow.source.to_string(),
            "source_node": self.named(flow.source),
            "destination": flow.destination.to_string(),
            "destination_node": self.named(flow.destination),
            "protocol": frame::protocol_name(flow.protocol),
            "port": flow.destination_port,
        })
    }

    /// Whether a conversation from `source` to `destination` over
    /// `protocol` was refused, and by which rule (`None` inside: the
    /// default). What a light machine's terminal says when nobody answered.
    pub fn refused(
        &self,
        source: IpAddr,
        destination: IpAddr,
        protocol: u8,
    ) -> Option<Option<String>> {
        self.flows
            .iter()
            .find(|(flow, seen)| {
                flow.source == source
                    && flow.destination == destination
                    && flow.protocol == protocol
                    && !seen.allowed
            })
            .map(|(_, seen)| seen.rule.clone())
    }

    /// The limited rule that cut a conversation from `source` to
    /// `destination` over `protocol`, when one did.
    pub fn limited(&self, source: IpAddr, destination: IpAddr, protocol: u8) -> Option<&Rule> {
        let id = self
            .flows
            .iter()
            .find(|(flow, seen)| {
                flow.source == source
                    && flow.destination == destination
                    && flow.protocol == protocol
                    && seen.cut.is_some()
            })
            .and_then(|(_, seen)| seen.rule.as_ref())?;
        self.live.rules.iter().find(|rule| &rule.id == id)
    }

    /// The conversations remembered, busiest first.
    pub fn flows(&self) -> Vec<serde_json::Value> {
        // A conversation through NAT is also judged as the router it left
        // as, or the router's door it came in by: the same conversation,
        // told once, as the machines have it.
        let mut flows: Vec<(&Flow, &Seen)> = self
            .flows
            .iter()
            .filter(|(flow, _)| !self.is_router(flow.source) && !self.is_router(flow.destination))
            .collect();
        flows.sort_by_key(|(_, seen)| Reverse(seen.packets));
        flows
            .into_iter()
            .map(|(flow, seen)| {
                let mut data = self.describe(*flow);
                data.insert(
                    "action".into(),
                    (if seen.allowed { "allow" } else { "deny" }).into(),
                );
                data.insert("packets".into(), seen.packets.into());
                data.insert("source_port".into(), flow.source_port.into());
                data.insert(
                    "rule".into(),
                    seen.rule
                        .clone()
                        .map_or(serde_json::Value::Null, Into::into),
                );
                data.insert("answer".into(), seen.answer.into());
                data.insert("limited".into(), seen.cut.is_some().into());
                data.insert("replied".into(), seen.replied.into());
                data.insert("reset".into(), seen.reset.into());
                if let Some(trail) = self.paths.get(flow) {
                    data.insert("path".into(), trail.value());
                }
                data.insert(
                    "quiet_ms".into(),
                    (seen.last.elapsed().as_millis() as u64).into(),
                );
                serde_json::Value::Object(data)
            })
            .collect()
    }

    /// Every name looked up, most asked first: who asked, what, how often,
    /// and what the network answered (`None`: it knows no such name).
    pub fn lookups(&self) -> Vec<serde_json::Value> {
        let mut lookups: Vec<_> = self.lookups.iter().collect();
        lookups.sort_by_key(|(_, lookup)| Reverse(lookup.count));
        lookups
            .into_iter()
            .map(|((node, name), lookup)| {
                serde_json::json!({
                    "node": node,
                    "name": name,
                    "count": lookup.count,
                    "answer": lookup.answer.map(|a| a.to_string()),
                    "rule": lookup.rule,
                })
            })
            .collect()
    }

    /// Add a rule by name at the end; its id.
    pub fn add_name(&mut self, mut rule: NameRule) -> String {
        let next = self
            .live
            .names
            .iter()
            .filter_map(|r| r.id.strip_prefix('n')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        rule.id = format!("n{next}");
        rule.uses = 0;
        let id = rule.id.clone();
        self.live.names.push(rule);
        self.generation += 1;
        id
    }

    /// Remove a rule by name; whether it was there.
    pub fn remove_name(&mut self, id: &str) -> bool {
        let before = self.live.names.len();
        self.live.names.retain(|r| r.id != id);
        self.generation += 1;
        before != self.live.names.len()
    }

    /// The rule by name that hides or points `name` for `source`, if one does.
    pub fn answers_name(&self, name: &str, source: IpAddr) -> Option<&NameRule> {
        self.name_rule(name, source, false)
            .map(|i| &self.live.names[i])
    }

    /// The first rule by name for `name` asked by `source`, of the kind
    /// asked: one that answers names (hide, point), or one that cuts.
    fn name_rule(&self, name: &str, source: IpAddr, cuts: bool) -> Option<usize> {
        self.live.names.iter().position(|rule| {
            (rule.action == NameAction::Cut) == cuts
                && name_fits(&rule.name, name)
                && self.matches(&rule.source, source)
        })
    }

    /// The first free port of `device`: no machine and no cable on it.
    pub fn free_port(&self, device: usize) -> u16 {
        (0..=u16::MAX)
            .find(|port| {
                let end = End {
                    device,
                    port: *port,
                };
                self.nodes.iter().all(|n| n.at != end) && self.topology.across(end).is_none()
            })
            .unwrap_or(u16::MAX)
    }

    /// A device of `kind`, with no cable yet; its index.
    pub fn add_device(&mut self, kind: Kind, name: &str) -> usize {
        self.topology.devices.push(Device {
            name: name.to_owned(),
            kind,
        });
        self.topology.devices.len() - 1
    }

    /// A cable between two ports that have no machine on them; see
    /// [`Topology::link`] for what else it refuses.
    pub fn link(&mut self, a: End, b: End) -> Result<(), beatbox_reasons::Reason> {
        if self.nodes.iter().any(|n| n.at == a || n.at == b) {
            return Err(beatbox_reasons::reason!("network.port_taken"));
        }
        self.topology.link(a, b)
    }

    /// Call device `device` `new`. Its networks and routes go with it; nothing
    /// else names a device, so nothing else changes.
    pub fn rename_device(&mut self, device: usize, new: &str) {
        let old = std::mem::replace(&mut self.topology.devices[device].name, new.to_owned());
        if let Some(conf) = self.routers.remove(&old) {
            self.routers.insert(new.to_owned(), conf);
        }
    }

    /// Call node `index` `new`, everywhere it is named: its light machine,
    /// the cable's own traffic and counts, what it still speaks as, and the
    /// firewall rules written against its name (the ones in the settings
    /// a reset goes back to too). Its socket moves with it.
    pub fn rename_node(&mut self, index: usize, new: &str, room: &Path) {
        let old = std::mem::replace(&mut self.nodes[index].name, new.to_owned());
        self.nodes[index].port = room.join(format!("{new}.sock"));
        if let Some(light) = self.lights.remove(&old) {
            self.lights.insert(new.to_owned(), light);
        }
        if let Some(traffic) = self.node_traffic.remove(&old) {
            self.node_traffic.insert(new.to_owned(), traffic);
        }
        if let Some(frames) = self.node_frames.remove(&old) {
            self.node_frames.insert(new.to_owned(), frames);
        }
        if self.reserved.remove(&old) {
            self.reserved.insert(new.to_owned());
        }
        if let Some(at) = self.leases.remove(&old) {
            self.leases.insert(new.to_owned(), at);
        }
        if let Some(from) = self.moved_from.remove(&old) {
            self.moved_from.insert(new.to_owned(), from);
        }
        for capture in &mut self.captures {
            if capture.tap == crate::capture::Tap::Node(old.clone()) {
                capture.tap = crate::capture::Tap::Node(new.to_owned());
            }
        }
        for rules in [&mut self.live.rules, &mut self.baseline.rules] {
            for rule in rules {
                for end in [&mut rule.source, &mut rule.destination] {
                    if *end == old {
                        *end = new.to_owned();
                    }
                }
            }
        }
        for groups in [&mut self.live.groups, &mut self.baseline.groups] {
            for member in groups.values_mut().flatten() {
                if *member == old {
                    *member = new.to_owned();
                }
            }
        }
        self.forget_conversations();
    }

    /// Take a device away, and the cables on it. The machines on it move to
    /// the main router, which is never taken away: nothing is unplugged by
    /// removing what it was plugged into.
    pub fn remove_device(&mut self, device: usize) -> Result<(), beatbox_reasons::Reason> {
        if self.topology.router() == Some(device) {
            return Err(beatbox_reasons::reason!("network.last_router"));
        }
        if device >= self.topology.devices.len() {
            return Err(beatbox_reasons::reason!("network.no_such_device"));
        }
        if let Some(main) = self.topology.router() {
            for node in 0..self.nodes.len() {
                if self.nodes[node].at.device == device {
                    let port = self.free_port(main);
                    self.nodes[node].at = End { device: main, port };
                }
            }
        }
        self.topology
            .cables
            .retain(|(a, b)| a.device != device && b.device != device);
        let gone = self.topology.devices.remove(device);
        self.routers.remove(&gone.name);
        let shift = |end: End| End {
            device: if end.device > device {
                end.device - 1
            } else {
                end.device
            },
            port: end.port,
        };
        self.topology.modes = std::mem::take(&mut self.topology.modes)
            .into_iter()
            .filter(|(end, _)| end.device != device)
            .map(|(end, mode)| (shift(end), mode))
            .collect();
        for cable in &mut self.topology.cables {
            *cable = (shift(cable.0), shift(cable.1));
        }
        for node in &mut self.nodes {
            node.at = shift(node.at);
        }
        self.link_traffic = std::mem::take(&mut self.link_traffic)
            .into_iter()
            .filter(|((a, b), _)| a.device != device && b.device != device)
            .map(|((a, b), traffic)| (cable_key(shift(a), shift(b)), traffic))
            .collect();
        self.link_frames.clear();
        Ok(())
    }

    /// Take the cable at `end` out; whether there was one.
    pub fn unlink(&mut self, end: End) -> bool {
        let before = self.topology.cables.len();
        self.topology.cables.retain(|&(a, b)| a != end && b != end);
        self.link_traffic.retain(|(a, b), _| *a != end && *b != end);
        before != self.topology.cables.len()
    }

    /// Plug machine `node` into `end`, a port with nothing on it. Its cable
    /// to the network is the same socket; only where it plugs in changes.
    pub fn place(&mut self, node: usize, end: End) -> Result<(), beatbox_reasons::Reason> {
        if end.device >= self.topology.devices.len() {
            return Err(beatbox_reasons::reason!("network.no_such_device"));
        }
        let taken = self
            .nodes
            .iter()
            .enumerate()
            .any(|(i, n)| i != node && n.at == end)
            || self.topology.across(end).is_some();
        if taken {
            return Err(beatbox_reasons::reason!("network.port_taken"));
        }
        self.nodes[node].at = end;
        Ok(())
    }

    /// What `end` carries from now on.
    pub fn configure_port(&mut self, end: End, mode: Mode) {
        if mode == Mode::default() {
            self.topology.modes.remove(&end);
        } else {
            self.topology.modes.insert(end, mode);
        }
    }

    /// The ports of `device` that lead somewhere, machines first in their
    /// order, then cables.
    fn ports_in_use(&self, device: usize) -> Vec<u16> {
        let mut ports: Vec<u16> = self
            .nodes
            .iter()
            .filter(|n| n.at.device == device)
            .map(|n| n.at.port)
            .collect();
        ports.extend(self.topology.cabled(device).map(|(port, _)| port));
        ports
    }

    /// One frame from node `from`: what to send where, and what to say.
    pub fn handle(&mut self, from: usize, bytes: &[u8]) -> (Vec<Out>, Vec<Said>) {
        let mut out = Vec::new();
        let mut said = Vec::new();
        self.counters.received += 1;
        self.costs.clear();
        let start = self.nodes[from].at;
        let name = self.nodes[from].name.clone();
        self.node_frames.entry(name.clone()).or_default().0 += 1;
        self.tap(&crate::capture::Tap::Node(name.clone()), bytes);
        self.trail = frame::untag(bytes)
            .map_or_else(|| flow_of(bytes), |(_, plain)| flow_of(&plain))
            .filter(|flow| match flow.destination {
                IpAddr::V4(a) => !a.is_broadcast() && !a.is_multicast(),
                IpAddr::V6(a) => !a.is_multicast(),
            })
            .map(|flow| Trail {
                key: Some(flow),
                flow: Some(flow),
                ..Trail::default()
            });
        let cable = self.node_traffic.get(&name).copied().unwrap_or_default();
        // The port the machine is plugged into decides its VLAN: a tag on an
        // access port, or one a trunk does not carry, is not taken.
        match self.topology.mode(start).ingress(bytes) {
            Some((vlan, plain)) if self.admits(from, &plain) => {
                if let Some(IpAddr::V6(source)) = frame::ipv6(&plain).map(|p| p.flow.source) {
                    self.learn6(from, source);
                }
                self.travel(
                    VecDeque::from([(start, vlan, plain, cable)]),
                    &mut out,
                    &mut said,
                );
            }
            _ => self.counters.refused += 1,
        }
        self.keep_trail();
        (out, said)
    }

    /// The way of the frame just handled, kept for its conversation when it
    /// is the first, or when it ended somewhere else than the last one.
    fn keep_trail(&mut self) {
        let Some(trail) = self.trail.take() else {
            return;
        };
        // A forwarded conversation is the one the machine behind the router
        // has; any other, the one its first packet began.
        let forwarded = trail.nat.as_ref().is_some_and(|nat| nat.kind == "forward");
        let Some(flow) = (if forwarded { trail.flow } else { trail.key }) else {
            return;
        };
        if !self.flows.contains_key(&flow) || trail.hops.is_empty() {
            return;
        }
        let changed = self
            .paths
            .get(&flow)
            .is_none_or(|kept| kept.reached != trail.reached || kept.judged != trail.judged);
        if changed {
            if self.paths.len() >= FLOWS_KEPT {
                let flows = &self.flows;
                self.paths.retain(|flow, _| flows.contains_key(flow));
            }
            self.paths.insert(flow, trail);
        }
    }

    /// Whether router `router` hides its network in `vlan`.
    pub fn hides(&self, router: usize, vlan: u16) -> bool {
        self.conf(router)
            .is_some_and(|conf| conf.hidden.contains(&vlan))
    }

    /// Hide router `router`'s network in `vlan`, or show it again. A
    /// network another router has an address in is the way between them:
    /// hiding it would cut the routes, and is refused.
    pub fn set_hidden(
        &mut self,
        router: usize,
        vlan: u16,
        on: bool,
    ) -> Result<(), beatbox_reasons::Reason> {
        use beatbox_reasons::reason;
        let name = self.topology.devices[router].name.clone();
        let Some(side) = self.side(router, vlan) else {
            return Err(reason!(
                "network.nat_no_network",
                router = name,
                vlan = vlan
            ));
        };
        let shared = self
            .every_side()
            .iter()
            .any(|(other, _, theirs)| *other != router && theirs.range == side.range);
        if on && shared {
            return Err(reason!("network.nat_shared", router = name, vlan = vlan));
        }
        let conf = self.routers.entry(name).or_default();
        if on {
            conf.hidden.insert(vlan);
        } else {
            conf.hidden.remove(&vlan);
        }
        self.forget_nat();
        Ok(())
    }

    /// A door on router `router` to one of its machines; its id.
    pub fn add_forward(
        &mut self,
        router: usize,
        mut forward: Forward,
    ) -> Result<String, beatbox_reasons::Reason> {
        use beatbox_reasons::reason;
        if self.node(&forward.to).is_none() {
            return Err(reason!("network.no_such_node", node = forward.to));
        }
        let name = self.topology.devices[router].name.clone();
        let conf = self.routers.entry(name).or_default();
        if conf
            .forwards
            .iter()
            .any(|f| f.protocol == forward.protocol && f.port == forward.port)
        {
            return Err(reason!("network.forward_taken", port = forward.port));
        }
        let next = conf
            .forwards
            .iter()
            .filter_map(|f| f.id.strip_prefix('f')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        forward.id = format!("f{next}");
        let id = forward.id.clone();
        conf.forwards.push(forward);
        self.generation += 1;
        Ok(id)
    }

    /// Take a door away; whether there was one.
    pub fn remove_forward(&mut self, router: usize, id: &str) -> bool {
        let name = self.topology.devices[router].name.clone();
        let Some(conf) = self.routers.get_mut(&name) else {
            return false;
        };
        let before = conf.forwards.len();
        conf.forwards.retain(|f| f.id != id);
        let gone = before != conf.forwards.len();
        self.forget_nat();
        gone
    }

    /// A packet for router `router`'s own address that is really for a
    /// machine behind it -- the answer to one a hidden machine began, or one
    /// a forward lets in -- and that machine's address and port; whether it
    /// came by a forward.
    fn inward(&mut self, router: usize, flow: Flow) -> Option<((Ipv4Addr, u16), bool)> {
        let key = (
            flow.protocol,
            flow.destination4(),
            flow.destination_port,
            flow.source4(),
            flow.source_port,
        );
        if let Some(inside) = self.nat.get(&key) {
            return Some((*inside, false));
        }
        let forward = self
            .conf(router)?
            .forwards
            .iter()
            .find(|f| f.protocol == flow.protocol && f.port == flow.destination_port)?;
        let (id, inside) = (
            forward.id.clone(),
            (self.node(&forward.to)?.address, forward.to_port),
        );
        self.bound_nat();
        let first = self.unforward.insert(
            (
                flow.protocol,
                inside.0,
                inside.1,
                flow.source4(),
                flow.source_port,
            ),
            (flow.destination4(), flow.destination_port),
        );
        // A new conversation through this door, not another packet of one.
        if first.is_none() {
            let name = self.topology.devices[router].name.clone();
            if let Some(forward) = self
                .routers
                .get_mut(&name)
                .and_then(|conf| conf.forwards.iter_mut().find(|f| f.id == id))
            {
                forward.uses += 1;
                forward.last = Some(Instant::now());
            }
        }
        Some((inside, true))
    }

    /// An ICMP error (time exceeded, unreachable) for the router's address
    /// that is about a packet a hidden machine sent out as the router: the
    /// same error, for the machine, quoting its packet as the machine sent
    /// it -- so `traceroute` from behind the router sees past it.
    fn error_inward(&self, flow: Flow, ip: &[u8]) -> Option<(Vec<u8>, Flow)> {
        if !icmp_type(ip)
            .is_some_and(|kind| matches!(kind, frame::ICMP_UNREACHABLE | frame::ICMP_TIME_EXCEEDED))
        {
            return None;
        }
        let quoted_at = usize::from(ip[0] & 0x0f) * 4 + 8;
        let quoted = ip.get(quoted_at..)?;
        let inner = usize::from(*quoted.first()? & 0x0f) * 4;
        let protocol = *quoted.get(9)?;
        let address = |at: usize| {
            Some(Ipv4Addr::new(
                *quoted.get(at)?,
                quoted[at + 1],
                quoted[at + 2],
                quoted[at + 3],
            ))
        };
        let (source, destination) = (address(12)?, address(16)?);
        let port = |at: usize| Some(u16::from_be_bytes([*quoted.get(at)?, *quoted.get(at + 1)?]));
        // Where the machine's own port sits in what is quoted: a TCP or UDP
        // source port, or a ping's identifier.
        let (at, port, back) = match protocol {
            frame::TCP | frame::UDP => (inner, port(inner)?, port(inner + 2)?),
            frame::ICMP if quoted.get(inner) == Some(&8) => {
                let id = port(inner + 4)?;
                (inner + 4, id, id)
            }
            _ => return None,
        };
        let inside = *self.nat.get(&(protocol, source, port, destination, back))?;
        let mut packet = ip.to_vec();
        let start = quoted_at;
        packet[start + 12..start + 16].copy_from_slice(&inside.0.octets());
        packet[start + at..start + at + 2].copy_from_slice(&inside.1.to_be_bytes());
        packet[start + 10..start + 12].copy_from_slice(&[0, 0]);
        let sum = frame::checksum(&packet[start..start + inner]);
        packet[start + 10..start + 12].copy_from_slice(&sum.to_be_bytes());
        // The outer destination, and the outer and ICMP checksums with it.
        frame::rewrite(&mut packet, None, Some((inside.0, 0)));
        Some((
            packet,
            moved(flow, None, Some((inside.0, flow.destination_port))),
        ))
    }

    /// A packet leaving router `router` from `vlan` by `out`: the answer of
    /// a forwarded conversation, as the router's door it came in by; from a
    /// hidden network to one that is not, as the router.
    fn outward(&mut self, router: usize, vlan: u16, out: u16, flow: Flow, packet: &mut [u8]) {
        let name = self.topology.devices[router].name.clone();
        let door = self
            .unforward
            .get(&(
                flow.protocol,
                flow.source4(),
                flow.source_port,
                flow.destination4(),
                flow.destination_port,
            ))
            .copied();
        let (from, to, kind) = if let Some(door) = door {
            ((flow.source4(), flow.source_port), door, "unforward")
        } else if self.hides(router, vlan) && !self.hides(router, out) {
            let Some(outside) = self.side(router, out).map(|side| side.address) else {
                return;
            };
            let echo = flow.protocol == frame::ICMP;
            let inside_key = (
                flow.protocol,
                flow.source4(),
                flow.source_port,
                flow.destination4(),
                flow.destination_port,
            );
            let port = match self.nat_out.get(&inside_key) {
                Some((_, port)) => *port,
                None => {
                    // The machine's own port, unless another machine
                    // already leaves on it to the same place.
                    let mut port = flow.source_port;
                    let taken = |nat: &HashMap<NatKey, (Ipv4Addr, u16)>, port: u16| {
                        let back = if echo { port } else { flow.destination_port };
                        nat.get(&(flow.protocol, outside, port, flow.destination4(), back))
                            .is_some_and(|inside| *inside != (flow.source4(), flow.source_port))
                    };
                    while taken(&self.nat, port) {
                        port = port.wrapping_add(1).max(1024);
                    }
                    self.bound_nat();
                    let back = if echo { port } else { flow.destination_port };
                    self.nat.insert(
                        (flow.protocol, outside, port, flow.destination4(), back),
                        (flow.source4(), flow.source_port),
                    );
                    self.nat_out.insert(inside_key, (outside, port));
                    port
                }
            };
            ((flow.source4(), flow.source_port), (outside, port), "hide")
        } else {
            return;
        };
        frame::rewrite(packet, Some(to), None);
        let next = moved(flow, Some(to), None);
        if let Some(trail) = &mut self.trail
            && trail.flow == Some(flow)
        {
            trail.flow = Some(next);
            if kind == "hide" {
                trail.nat = Some(NatStep {
                    router: name,
                    kind,
                    from,
                    to,
                });
            }
        }
    }

    /// NAT keeps no more than the conversations it is kept beside.
    fn bound_nat(&mut self) {
        if self.nat.len() + self.unforward.len() >= FLOWS_KEPT {
            self.nat.clear();
            self.nat_out.clear();
            self.unforward.clear();
        }
    }

    /// Whether the IPv4 frame `bytes` is the one being followed.
    fn followed(&self, bytes: &[u8]) -> bool {
        self.trail
            .as_ref()
            .is_some_and(|trail| trail.flow.is_some() && flow_of(bytes) == trail.flow)
    }

    /// Every running capture on `tap` sees `wire`.
    fn tap(&mut self, tap: &crate::capture::Tap, wire: &[u8]) {
        for capture in &mut self.captures {
            if capture.tap == *tap {
                capture.record(wire);
            }
        }
    }

    /// Port security, where the machine plugs in, whatever it plugs into: a
    /// node sends as itself or not at all, so no rule written against its
    /// name can be walked around by changing its MAC or its address. Read
    /// on the frame without its VLAN tag, so a tag hides nothing.
    fn admits(&self, from: usize, bytes: &[u8]) -> bool {
        let me = &self.nodes[from];
        let (mac, address) = (me.mac, me.address);
        if frame::ethertype(bytes).is_none() || frame::source(bytes) != mac {
            return false;
        }
        match frame::ethertype(bytes) {
            Some(frame::ETHER_ARP) => frame::arp(bytes).is_some_and(|arp| {
                let moved = self.moved_from.get(&me.name) == Some(&arp.sender_ip)
                    && self.is_router(arp.target_ip.into());
                arp.sender_mac == mac
                    && (arp.sender_ip == address || arp.sender_ip.is_unspecified() || moved)
            }),
            Some(frame::ETHER_IPV4) => frame::ipv4(bytes).is_some_and(|packet| {
                let flow = packet.flow;
                let dhcp = flow.protocol == frame::UDP && flow.destination_port == 67;
                dhcp || flow.source == address
            }),
            // From its own link-local address, or one no other machine
            // holds; nothing yet (`::`) while it checks an address is free.
            Some(frame::ETHER_IPV6) => frame::ipv6(bytes).is_some_and(|packet| {
                matches!(packet.flow.source, IpAddr::V6(source)
                    if source.is_unspecified()
                        || self.addresses6.get(&source).is_none_or(|name| *name == me.name)
                        && self.holder(source.into()).is_none_or(|n| n == from))
            }),
            // A tag inside a tag is not a frame this network carries.
            Some(frame::ETHER_VLAN) => false,
            // Anything else -- IPv6, a protocol of a lab's own -- is carried;
            // where it crosses a router the firewall's default decides it.
            _ => true,
        }
    }

    /// Frames entering devices at ports, each with its VLAN and without a
    /// tag, until every copy has reached a machine or nowhere.
    fn travel(&mut self, mut queue: VecDeque<Inside>, out: &mut Vec<Out>, said: &mut Vec<Said>) {
        // A tree carries a frame through each device at most once per copy;
        // this only stops a mistake from spinning.
        let mut budget = 64 * (self.topology.devices.len() + 1);
        while let Some((at, vlan, bytes, cost)) = queue.pop_front() {
            if budget == 0 {
                self.counters.lost += 1;
                break;
            }
            budget -= 1;
            if self.followed(&bytes) {
                let device = &self.topology.devices[at.device];
                let router = device.kind == Kind::Router;
                let hop = Hop {
                    device: device.name.clone(),
                    kind: if router { "router" } else { "switch" },
                    port: at.port,
                    vlan,
                    ttl: router
                        .then(|| {
                            let v6 = frame::ethertype(&bytes) == Some(frame::ETHER_IPV6);
                            bytes.get(if v6 { 21 } else { 22 }).copied()
                        })
                        .flatten(),
                    ms: self.live.traffic.then(cost).latency_ms,
                };
                if let Some(trail) = &mut self.trail {
                    trail.hops.push(hop);
                }
            }
            let sent = match self.topology.devices.get(at.device).map(|d| d.kind) {
                Some(Kind::Router) => self.route(at, vlan, &bytes, said),
                Some(Kind::Switch) => self
                    .switch(at, &bytes)
                    .into_iter()
                    .map(|(port, bytes)| (port, vlan, bytes))
                    .collect(),
                None => continue,
            };
            for (port, vlan, bytes) in sent {
                let end = End {
                    device: at.device,
                    port,
                };
                self.exit(end, vlan, &bytes, cost, &mut queue, out);
            }
        }
    }

    /// A frame of `vlan` leaving through `end`, as that port puts it on the
    /// wire (tagged, untagged, or not at all), to the machine there or along
    /// the cable to the port at the other end, which reads it its own way.
    /// Two ends that disagree -- different access VLANs, another native --
    /// carry it into the wrong VLAN, as two real switches would.
    fn exit(
        &mut self,
        end: End,
        vlan: u16,
        bytes: &[u8],
        cost: Traffic,
        queue: &mut VecDeque<Inside>,
        out: &mut Vec<Out>,
    ) {
        let Some(wire) = self.topology.mode(end).egress(vlan, bytes) else {
            return;
        };
        if let Some(node) = self.nodes.iter().position(|n| n.at == end) {
            let name = &self.nodes[node].name;
            let cable = self.node_traffic.get(name).copied().unwrap_or_default();
            let total = self.live.traffic.then(cost).then(cable);
            self.costs.push(total);
            self.node_frames.entry(name.clone()).or_default().1 += 1;
            let name = name.clone();
            if self.followed(bytes)
                && let Some(trail) = &mut self.trail
            {
                trail.reached = Some(name.clone());
                trail.ms = total.latency_ms;
            }
            self.tap(&crate::capture::Tap::Node(name), &wire);
            out.push((node, wire));
        } else if let Some(next) = self.topology.across(end)
            && let Some((vlan, plain)) = self.topology.mode(next).ingress(&wire)
        {
            let key = cable_key(end, next);
            *self.link_frames.entry(key).or_default() += 1;
            self.tap(&crate::capture::Tap::Link(key), &wire);
            let crossed = self.link_traffic.get(&key).copied().unwrap_or_default();
            queue.push_back((next, vlan, plain, cost.then(crossed)));
        }
    }

    /// A switch: by destination MAC to the one port that leads there, or to
    /// every other port for broadcast and multicast; only ports that carry
    /// the frame's VLAN let it out (`exit`). It judges nothing.
    fn switch(&mut self, at: End, bytes: &[u8]) -> Vec<(u16, Vec<u8>)> {
        let to = frame::destination(bytes);
        if frame::is_group(to) {
            return self
                .ports_in_use(at.device)
                .into_iter()
                .filter(|port| *port != at.port)
                .map(|port| (port, bytes.to_vec()))
                .collect();
        }
        let port = if let Some(router) = self.router_by_mac(to) {
            self.topology.toward(at.device, router)
        } else {
            self.nodes
                .iter()
                .find(|n| n.mac == to)
                .and_then(|n| self.topology.toward_end(at.device, n.at))
        };
        match port {
            Some(port) if port != at.port => vec![(port, bytes.to_vec())],
            _ => Vec::new(),
        }
    }

    /// A router: what is for one of its addresses is answered back the way
    /// it came; what is sent to it for elsewhere is routed (`forward`); what
    /// stays in one network but crosses its ports is judged and bridged. A
    /// VLAN it has no network in is only bridged.
    fn route(
        &mut self,
        at: End,
        vlan: u16,
        bytes: &[u8],
        said: &mut Vec<Said>,
    ) -> Vec<(u16, u16, Vec<u8>)> {
        let router = at.device;
        let me = self.router_mac(router);
        let sender = frame::source(bytes);
        // Who sent it: a machine (port security at its own port made the
        // MAC its proof), or another router passing a packet on.
        let from = self.nodes.iter().position(|n| n.mac == sender);
        if from.is_none() && self.router_by_mac(sender).is_none() {
            self.counters.refused += 1;
            return Vec::new();
        }
        let gateway = self.side(router, vlan).map(|side| side.address);
        let to = frame::destination(bytes);
        // Answers go to the one who asked, back through the port it came by.
        let mut answers: Vec<Vec<u8>> = Vec::new();
        let mut sent = Vec::new();
        let mut routed = Vec::new();
        match frame::ethertype(bytes) {
            Some(frame::ETHER_ARP) => {
                let Some(arp) = frame::arp(bytes) else {
                    return Vec::new();
                };
                // A machine that moved still asks for its old gateway, to
                // renew there and be told its new address.
                let moved = from.is_some_and(|f| {
                    self.moved_from.get(&self.nodes[f].name) == Some(&arp.sender_ip)
                }) && self.owner(arp.target_ip) == Some(router);
                if arp.operation == 1 && (Some(arp.target_ip) == gateway || moved) {
                    answers.push(frame::arp_reply(&arp, me));
                } else {
                    self.bridge(at, to, bytes, &mut sent);
                }
            }
            Some(frame::ETHER_IPV4) => {
                let Some(packet) = frame::ipv4(bytes) else {
                    return Vec::new();
                };
                let flow = packet.flow;
                let dhcp = flow.protocol == frame::UDP && flow.destination_port == 67;
                let mine = to == me || self.owner(flow.destination4()) == Some(router);
                if (dhcp || mine) && gateway.is_none() {
                    self.counters.unroutable += 1;
                } else if dhcp {
                    if let Some(from) = from {
                        self.dhcp(router, from, vlan, &packet, &mut answers);
                    }
                } else if mine {
                    let ip = ip_alone(bytes);
                    if self.owner(flow.destination4()) == Some(router) {
                        // An answer to a hidden machine, or a forward: on
                        // to the machine behind; anything else is for the
                        // router itself.
                        if let Some((packet, next)) = self.error_inward(flow, ip) {
                            // A router out there telling a hidden machine
                            // its packet died: told to the machine, about
                            // its own packet.
                            self.forward(
                                router,
                                vlan,
                                sender,
                                next,
                                &packet,
                                true,
                                &mut answers,
                                &mut routed,
                                said,
                            );
                        } else {
                            match self.inward(router, flow) {
                                Some((inside, forwarded)) => {
                                    let mut packet = ip.to_vec();
                                    frame::rewrite(&mut packet, None, Some(inside));
                                    let next = moved(flow, None, Some(inside));
                                    let name = self.topology.devices[router].name.clone();
                                    if let Some(trail) = &mut self.trail
                                        && trail.flow == Some(flow)
                                    {
                                        trail.flow = Some(next);
                                        if forwarded {
                                            trail.nat = Some(NatStep {
                                                router: name,
                                                kind: "forward",
                                                from: (flow.destination4(), flow.destination_port),
                                                to: inside,
                                            });
                                        }
                                    }
                                    self.forward(
                                        router,
                                        vlan,
                                        sender,
                                        next,
                                        &packet,
                                        true,
                                        &mut answers,
                                        &mut routed,
                                        said,
                                    );
                                }
                                None => {
                                    self.gateway(router, sender, &packet, ip, &mut answers, said)
                                }
                            }
                        }
                    } else {
                        self.forward(
                            router,
                            vlan,
                            sender,
                            flow,
                            ip,
                            false,
                            &mut answers,
                            &mut routed,
                            said,
                        );
                    }
                } else if from.is_none() {
                    // Another router's frame passing through, as a switch would.
                    self.bridge(at, to, bytes, &mut sent);
                } else {
                    // To one node, and it has to be the node the address
                    // says: a frame to B's MAC carrying C's address would be
                    // judged as A to C and delivered to B.
                    let honest = frame::is_group(to)
                        || self
                            .nodes
                            .iter()
                            .any(|n| n.mac == to && n.address == flow.destination);
                    let ip = ip_alone(bytes);
                    if !honest {
                        self.counters.refused += 1;
                    } else {
                        match self.police(flow, ip, said) {
                            Verdict::Allow => self.bridge(at, to, bytes, &mut sent),
                            verdict => {
                                self.counters.blocked += 1;
                                if verdict == Verdict::Reject && !frame::is_group(to) {
                                    answers.push(self.refusal(router, sender, flow, ip));
                                }
                            }
                        }
                    }
                }
            }
            Some(frame::ETHER_IPV6) => {
                self.route6(
                    at,
                    vlan,
                    bytes,
                    from,
                    &mut answers,
                    &mut sent,
                    &mut routed,
                    said,
                );
            }
            other => {
                if self.judge_other(other.unwrap_or_default(), from, to) {
                    self.bridge(at, to, bytes, &mut sent);
                } else {
                    self.counters.blocked += 1;
                }
            }
        }
        sent.extend(answers.into_iter().map(|bytes| (at.port, bytes)));
        let mut all: Vec<_> = sent
            .into_iter()
            .map(|(port, bytes)| (port, vlan, bytes))
            .collect();
        all.extend(routed);
        all
    }

    /// An IPv6 frame at a router. Neighbour discovery is never judged: the
    /// router answers what is about it (a router solicitation, a neighbour
    /// solicitation for its address) and carries the rest on as a switch
    /// would. What is for the router is answered (ping); what is sent to it
    /// for elsewhere is routed (`forward6`); what stays in one network but
    /// crosses its ports is judged and bridged, as IPv4 is.
    #[allow(clippy::too_many_arguments)]
    fn route6(
        &mut self,
        at: End,
        vlan: u16,
        bytes: &[u8],
        from: Option<usize>,
        answers: &mut Vec<Vec<u8>>,
        sent: &mut Vec<(u16, Vec<u8>)>,
        routed: &mut Vec<(u16, u16, Vec<u8>)>,
        said: &mut Vec<Said>,
    ) {
        let router = at.device;
        let me = self.router_mac(router);
        let sender = frame::source(bytes);
        let to = frame::destination(bytes);
        let Some(packet) = frame::ipv6(bytes) else {
            return;
        };
        let flow = packet.flow;
        let (IpAddr::V6(source), IpAddr::V6(destination)) = (flow.source, flow.destination) else {
            return;
        };
        let icmp = (flow.protocol == frame::ICMPV6)
            .then(|| packet.payload.first().copied())
            .flatten();
        let side = self
            .side(router, vlan)
            .filter(|_| self.speaks6(router, vlan));
        if icmp.is_some_and(frame::is_discovery) {
            if let (Some(side), Some(kind)) = (side, icmp) {
                let mine = [gateway6(side.range), frame::link_local(me)];
                let back = if source.is_unspecified() {
                    frame::ALL_NODES
                } else {
                    source
                };
                if kind == frame::ROUTER_SOLICIT && from.is_some() {
                    let message = frame::router_advert(prefix6(side.range), me);
                    let packet = frame::icmpv6_packet(frame::link_local(me), back, 255, message);
                    answers.push(frame::ethernet(sender, me, frame::ETHER_IPV6, &packet));
                } else if kind == frame::NEIGHBOR_SOLICIT
                    && let Some(target) = frame::solicited(packet.payload)
                    && mine.contains(&target)
                {
                    let message = frame::neighbor_advert(target, me);
                    let packet = frame::icmpv6_packet(target, back, 255, message);
                    answers.push(frame::ethernet(sender, me, frame::ETHER_IPV6, &packet));
                }
            }
            if to != me {
                self.bridge(at, to, bytes, sent);
            }
            return;
        }
        let ip = packet_alone(bytes);
        if to == me || self.owner6(destination) == Some(router) {
            if side.is_none() {
                self.counters.unroutable += 1;
            } else if self.owner6(destination) == Some(router) {
                match frame::echo6_reply(packet.payload).filter(|_| icmp.is_some()) {
                    Some(echo) => {
                        let packet = frame::icmpv6_packet(destination, source, 64, echo);
                        answers.push(frame::ethernet(sender, me, frame::ETHER_IPV6, &packet));
                    }
                    None => self.counters.unroutable += 1,
                }
            } else {
                self.forward6(router, vlan, sender, flow, ip, icmp, answers, routed, said);
            }
        } else if from.is_none() {
            // Another router's frame passing through, as a switch would.
            self.bridge(at, to, bytes, sent);
        } else {
            // To one node, and not to an address another machine holds.
            let honest = frame::is_group(to)
                || self
                    .nodes
                    .iter()
                    .position(|n| n.mac == to)
                    .is_some_and(|node| {
                        self.holder(flow.destination)
                            .is_none_or(|holder| holder == node)
                    });
            if !honest {
                self.counters.refused += 1;
                return;
            }
            match self.police(flow, ip, said) {
                Verdict::Allow => self.bridge(at, to, bytes, sent),
                verdict => {
                    self.counters.blocked += 1;
                    if verdict == Verdict::Reject && !frame::is_group(to) {
                        answers.push(self.refusal(router, sender, flow, ip));
                    }
                }
            }
        }
    }

    /// An IPv6 packet router `router` was given for somewhere else: on to
    /// the machine that holds the address, on one of its networks that
    /// speak IPv6, or to the next router, the way IPv4 goes to the network
    /// the /64 was made from (`v4_of6`). Judged by the firewall and one hop
    /// older; its hops spent, "time exceeded" (what `traceroute` reads); no
    /// way there, "unreachable".
    #[allow(clippy::too_many_arguments)]
    fn forward6(
        &mut self,
        router: usize,
        vlan: u16,
        sender: Mac,
        flow: Flow,
        ip: &[u8],
        icmp: Option<u8>,
        answers: &mut Vec<Vec<u8>>,
        routed: &mut Vec<(u16, u16, Vec<u8>)>,
        said: &mut Vec<Said>,
    ) {
        let (IpAddr::V6(source), IpAddr::V6(destination)) = (flow.source, flow.destination) else {
            return;
        };
        let me = self.router_mac(router);
        let here = self
            .side(router, vlan)
            .map_or(frame::link_local(me), |side| gateway6(side.range));
        let error = |kind, code| {
            let packet =
                frame::icmpv6_packet(here, source, 64, frame::icmpv6_error(kind, code, ip));
            frame::ethernet(sender, me, frame::ETHER_IPV6, &packet)
        };
        let related = icmp.is_some_and(|kind| {
            matches!(
                kind,
                frame::ICMPV6_UNREACHABLE | frame::ICMPV6_TIME_EXCEEDED
            )
        });
        let Some((port, out, mac)) = self.hop6(router, destination) else {
            self.counters.unroutable += 1;
            // No route, or nobody holds the address.
            let code = if self
                .sides6(router)
                .iter()
                .any(|(_, _, p)| in64(destination, *p))
            {
                3
            } else {
                0
            };
            answers.push(error(frame::ICMPV6_UNREACHABLE, code));
            return;
        };
        if !related {
            match self.police(flow, ip, said) {
                Verdict::Allow => {}
                verdict => {
                    self.counters.blocked += 1;
                    if verdict == Verdict::Reject {
                        answers.push(self.refusal(router, sender, flow, ip));
                    }
                    return;
                }
            }
        }
        let mut packet = ip.to_vec();
        if !frame::hop6(&mut packet) {
            answers.push(error(frame::ICMPV6_TIME_EXCEEDED, 0));
            return;
        }
        routed.push((
            port,
            out,
            frame::ethernet(mac, me, frame::ETHER_IPV6, &packet),
        ));
    }

    /// How router `router` reaches IPv6 `destination`: the port, the VLAN
    /// and the MAC -- the machine that holds it on one of its networks that
    /// speak IPv6, or the next router, which has to speak IPv6 on the
    /// network they share.
    fn hop6(&self, router: usize, destination: Ipv6Addr) -> Option<(u16, u16, Mac)> {
        if let Some((vlan, _, _)) = self
            .sides6(router)
            .into_iter()
            .find(|(_, _, prefix)| in64(destination, *prefix))
        {
            let node = self.holder(destination.into())?;
            let hop = self.hop_to(router, self.nodes[node].address)?;
            return (hop.1 == vlan).then_some(hop);
        }
        let next = self.next_hop(router, v4_of6(destination)?)?;
        let hop = self.hop_to(router, next)?;
        let other = self.router_by_mac(hop.2)?;
        (self.speaks6(router, hop.1) && self.speaks6(other, hop.1)).then_some(hop)
    }

    /// Turn IPv6 on or off for router `router`'s network in `vlan`.
    pub fn set_ipv6(
        &mut self,
        router: usize,
        vlan: u16,
        on: bool,
    ) -> Result<(), beatbox_reasons::Reason> {
        use beatbox_reasons::reason;
        let name = self.topology.devices[router].name.clone();
        if self.side(router, vlan).is_none() {
            return Err(reason!(
                "network.nat_no_network",
                router = name,
                vlan = vlan
            ));
        }
        let conf = self.routers.entry(name).or_default();
        if on {
            conf.ipv6.insert(vlan);
        } else {
            conf.ipv6.remove(&vlan);
        }
        self.generation += 1;
        Ok(())
    }

    /// Every router's advertisement on each network that speaks IPv6, to
    /// all the machines there: what a machine that started before IPv6 was
    /// turned on, or missed the answer to its solicitation, makes its
    /// address from. The switch's thread sends them now and then.
    pub fn announce(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        let mut said = Vec::new();
        self.costs.clear();
        self.trail = None;
        for router in self.router_devices() {
            let me = self.router_mac(router);
            for (vlan, _, prefix) in self.sides6(router) {
                let message = frame::router_advert(prefix, me);
                let packet =
                    frame::icmpv6_packet(frame::link_local(me), frame::ALL_NODES, 255, message);
                let bytes = frame::ethernet(
                    frame::multicast_mac(frame::ALL_NODES),
                    me,
                    frame::ETHER_IPV6,
                    &packet,
                );
                let mut queue = VecDeque::new();
                for port in self.ports_in_use(router) {
                    self.exit(
                        End {
                            device: router,
                            port,
                        },
                        vlan,
                        &bytes,
                        Traffic::default(),
                        &mut queue,
                        &mut out,
                    );
                }
                self.travel(queue, &mut out, &mut said);
            }
        }
        out
    }

    /// A packet router `router` was given for somewhere else: on to the next
    /// hop (`next_hop`, `hop_to`) -- a machine on one of its networks, or
    /// the next router -- judged by the firewall and one hop older. Its TTL
    /// spent: ICMP "time exceeded" back to whoever handed it over (what
    /// `traceroute` reads). No way there: "unreachable", or, at the main
    /// router, out to the internet (`leave`). An ICMP error passing through
    /// is part of a conversation already judged, and is not judged again.
    #[allow(clippy::too_many_arguments)]
    fn forward(
        &mut self,
        router: usize,
        vlan: u16,
        sender: Mac,
        flow: Flow,
        ip: &[u8],
        translated: bool,
        answers: &mut Vec<Vec<u8>>,
        routed: &mut Vec<(u16, u16, Vec<u8>)>,
        said: &mut Vec<Said>,
    ) {
        let here = self
            .side(router, vlan)
            .map_or(self.live.gateway(), |side| side.address);
        let me = self.router_mac(router);
        let error = |kind, code| {
            let icmp = frame::icmp_error(kind, code, ip);
            let packet = frame::ipv4_packet(here, flow.source4(), frame::ICMP, &icmp);
            frame::ethernet(sender, me, frame::ETHER_IPV4, &packet)
        };
        let icmp = icmp_type(ip);
        let related = icmp.is_some_and(|kind| {
            matches!(kind, frame::ICMP_UNREACHABLE | frame::ICMP_TIME_EXCEEDED)
        });
        let Some(next) = self.next_hop(router, flow.destination4()) else {
            if Some(router) == self.topology.router() && self.outside(flow.destination4()) {
                if self.leave(flow, ip, said) == Verdict::Reject {
                    answers.push(self.refusal(router, sender, flow, ip));
                }
            } else {
                self.counters.unroutable += 1;
                answers.push(error(frame::ICMP_UNREACHABLE, 0));
            }
            return;
        };
        let Some((port, out, mac)) = self.hop_to(router, next) else {
            self.counters.unroutable += 1;
            answers.push(error(frame::ICMP_UNREACHABLE, 1));
            return;
        };
        // Into a hidden network from outside it: only what NAT let in.
        if self.hides(router, out) && !self.hides(router, vlan) && !translated && !related {
            self.counters.blocked += 1;
            let name = self.topology.devices[router].name.clone();
            if let Some(trail) = &mut self.trail
                && trail.flow == Some(flow)
            {
                trail.nat = Some(NatStep {
                    router: name,
                    kind: "closed",
                    from: (flow.destination4(), flow.destination_port),
                    to: (flow.destination4(), flow.destination_port),
                });
            }
            return;
        }
        if !related {
            match self.police(flow, ip, said) {
                Verdict::Allow => {}
                verdict => {
                    self.counters.blocked += 1;
                    if verdict == Verdict::Reject {
                        answers.push(self.refusal(router, sender, flow, ip));
                    }
                    return;
                }
            }
        }
        let mut packet = ip.to_vec();
        if !related {
            self.outward(router, vlan, out, flow, &mut packet);
        }
        if !frame::hop(&mut packet) {
            answers.push(error(frame::ICMP_TIME_EXCEEDED, 0));
            return;
        }
        routed.push((
            port,
            out,
            frame::ethernet(mac, me, frame::ETHER_IPV4, &packet),
        ));
    }

    /// Between the router's own ports, as the switch it also is.
    fn bridge(&self, at: End, to: Mac, bytes: &[u8], sent: &mut Vec<(u16, Vec<u8>)>) {
        if frame::is_group(to) {
            for port in self.ports_in_use(at.device) {
                if port != at.port {
                    sent.push((port, bytes.to_vec()));
                }
            }
        } else if let Some(port) = self
            .nodes
            .iter()
            .find(|n| n.mac == to)
            .and_then(|n| self.topology.toward_end(at.device, n.at))
            .filter(|port| *port != at.port)
        {
            sent.push((port, bytes.to_vec()));
        }
    }

    /// A frame a router makes itself (an answer from outside) on its way to
    /// the machine `to`, handed over by the router that serves it.
    /// ponytail: an answer for a machine behind a second router is handed
    /// to it there, without crossing the routers between.
    fn send_down(
        &mut self,
        to: usize,
        mut bytes: Vec<u8>,
        out: &mut Vec<Out>,
        said: &mut Vec<Said>,
    ) {
        let at = self.nodes[to].at;
        let (router, vlan) = match self.serving(to) {
            Some((router, vlan, _)) => (router, vlan),
            None => {
                let Some(router) = self.topology.router() else {
                    return;
                };
                let vlan = match self.topology.mode(at) {
                    Mode::Access(vlan) | Mode::Trunk { native: vlan, .. } => vlan,
                };
                (router, vlan)
            }
        };
        let Some(port) = self.topology.toward_end(router, at) else {
            return;
        };
        if let Some(source) = bytes.get_mut(6..12) {
            source.copy_from_slice(&self.router_mac(router));
        }
        let mut queue = VecDeque::new();
        self.exit(
            End {
                device: router,
                port,
            },
            vlan,
            &bytes,
            Traffic::default(),
            &mut queue,
            out,
        );
        self.travel(queue, out, said);
    }

    /// The address node `from` holds, offered in the network of the VLAN
    /// the question came in; a different one asked for is refused (NAK), so
    /// a machine that moved learns its new address on renewing.
    ///
    /// A renewal is answered as it was asked: unicast to the address it
    /// holds, from the router address it asked. A client renewing listens
    /// only there (NetworkManager's does), so a broadcast answer would leave
    /// it waiting for the rebind, most of a lease later.
    fn dhcp(
        &mut self,
        router: usize,
        from: usize,
        vlan: u16,
        packet: &frame::Ipv4<'_>,
        out: &mut Vec<Vec<u8>>,
    ) {
        let Some(ask) = frame::dhcp_ask(frame::udp_data(packet.payload)) else {
            return;
        };
        // Only the router that serves the machine answers it.
        let Some((serving, served, side)) = self.serving(from) else {
            return;
        };
        let me = &self.nodes[from];
        let range = side.range;
        let settings = self.dhcp_of(serving, served);
        if (serving, served) != (router, vlan)
            || ask.client != me.mac
            || !host_in(me.address, range)
            || !settings.on
        {
            return;
        }
        let kind = match ask.kind {
            frame::DHCP_DISCOVER => frame::DHCP_OFFER,
            frame::DHCP_REQUEST if ask.requested.is_none_or(|r| r == me.address) => frame::DHCP_ACK,
            frame::DHCP_REQUEST => frame::DHCP_NAK,
            _ => return,
        };
        if kind == frame::DHCP_ACK {
            let name = me.name.clone();
            self.moved_from.remove(&name);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            self.leases.insert(name, now);
        }
        let me = &self.nodes[from];
        let gateway = side.address;
        let asked = packet.flow.destination4();
        let server = if self.owner(asked) == Some(router) {
            asked
        } else {
            gateway
        };
        // A refusal names the server that was asked; an offer or an ack, the
        // gateway of the network the machine is in.
        let named = if kind == frame::DHCP_NAK {
            server
        } else {
            gateway
        };
        let offer = frame::Offer {
            lease: settings.lease,
            dns: settings.dns.unwrap_or(gateway),
            domain: &self.live.domain,
        };
        let answer = frame::dhcp_answer(&ask, kind, me.address, named, range.1, &offer);
        let (mac, to) = match ask.held {
            Some(held) if !asked.is_broadcast() => (me.mac, held),
            _ => (BROADCAST, Ipv4Addr::BROADCAST),
        };
        let packet = frame::ipv4_packet(server, to, frame::UDP, &frame::udp(67, 68, &answer));
        out.push(frame::ethernet(
            mac,
            self.router_mac(router),
            frame::ETHER_IPV4,
            &packet,
        ));
    }

    /// What router `router` answers on its own addresses: ping, and names.
    /// The answer goes back to `sender`, the machine or router that handed
    /// the question over.
    fn gateway(
        &mut self,
        router: usize,
        sender: Mac,
        packet: &frame::Ipv4<'_>,
        ip: &[u8],
        out: &mut Vec<Vec<u8>>,
        said: &mut Vec<Said>,
    ) {
        let flow = packet.flow;
        let me = self.router_mac(router);
        let reply = |protocol, payload: &[u8]| {
            let ip = frame::ipv4_packet(flow.destination4(), flow.source4(), protocol, payload);
            frame::ethernet(sender, me, frame::ETHER_IPV4, &ip)
        };
        if flow.protocol == frame::ICMP {
            if let Some(echo) = frame::echo_reply(packet.payload) {
                out.push(reply(frame::ICMP, &echo));
            }
            return;
        }
        if flow.protocol == frame::UDP && flow.destination_port == 53 {
            let Some(question) = frame::dns_question(frame::udp_data(packet.payload)) else {
                return;
            };
            let ruled = self.name_rule(&question.name, flow.source, false);
            let action = ruled.map(|i| {
                self.live.names[i].uses += 1;
                self.live.names[i].action.clone()
            });
            let rule = ruled.map(|i| self.live.names[i].id.clone());
            let (address, address6) = match &action {
                Some(NameAction::Hide) => (None, None),
                Some(NameAction::Point(to)) => (
                    self.resolve(to).or_else(|| to.parse().ok()),
                    self.resolve6(to).or_else(|| to.parse().ok()),
                ),
                _ => (self.resolve(&question.name), self.resolve6(&question.name)),
            };
            let node = self
                .nodes
                .iter()
                .find(|n| n.address == flow.source4())
                .map_or_else(|| flow.source4().to_string(), |n| n.name.clone());
            let known = self.lookups.len() < LOOKUPS_KEPT
                || self
                    .lookups
                    .contains_key(&(node.clone(), question.name.clone()));
            // The resolvers' own "who answers for .local?" is not a lookup.
            let ours = question.kind == 6 && question.name == self.live.domain;
            let mut news = !ours;
            if known && !ours {
                let lookup = self
                    .lookups
                    .entry((node.clone(), question.name.clone()))
                    .or_default();
                // Said when it is news: a name asked for the first time, or
                // answered differently. A guest asking every few seconds is
                // counted, not repeated.
                news = lookup.count == 0 || lookup.answer != address || lookup.rule != rule;
                lookup.count += 1;
                lookup.answer = address;
                lookup.rule = rule.clone();
            }
            if news {
                said.push((
                    "dns.query",
                    data!({
                        "node": node,
                        "name": question.name,
                        "type": question.kind,
                        "answer": address.map(|a| a.to_string()),
                        "rule": rule,
                    }),
                ));
            }
            let hidden = action == Some(NameAction::Hide);
            let known = !hidden && (address.is_some() || self.knows(&question.name));
            if !known && !ours && self.live.internet && action.is_none() {
                // Asked of this computer's resolver; the answer comes back
                // from the gateway, through `arrive`.
                self.leaving.push(Leaving {
                    flow,
                    packet: ip.to_vec(),
                    resolve: true,
                });
                return;
            }
            let answers = if hidden {
                Vec::new()
            } else if matches!(action, Some(NameAction::Point(_))) {
                let found = match question.kind {
                    1 => address.map(frame::Record::A),
                    28 => address6.map(frame::Record::Aaaa),
                    _ => None,
                };
                found
                    .map(|r| (question.name.clone(), r))
                    .into_iter()
                    .collect()
            } else {
                self.answers(&question.name, question.kind)
            };
            let answer = frame::dns_reply(&question, known, &self.live.domain, &answers);
            let datagram = frame::udp(53, flow.source_port, &answer);
            out.push(reply(frame::UDP, &datagram));
            return;
        }
        self.counters.unroutable += 1;
    }

    /// A packet for somewhere outside, sent to the router. Out through the
    /// uplink when the router leads to the internet, the firewall lets it
    /// and it is UDP or TCP; nowhere otherwise.
    /// ponytail: no ICMP out yet; ping needs an ICMP socket, which an
    /// unprivileged process has only where the system allows it.
    /// What the firewall said: `Allow` when it went, or went nowhere.
    fn leave(&mut self, flow: Flow, ip: &[u8], said: &mut Vec<Said>) -> Verdict {
        let carried = flow.protocol == frame::UDP || flow.protocol == frame::TCP;
        if !self.live.internet || !self.outside(flow.destination4()) || !carried {
            self.counters.unroutable += 1;
            return Verdict::Allow;
        }
        let verdict = self.police(flow, ip, said);
        if verdict != Verdict::Allow {
            self.counters.blocked += 1;
            return verdict;
        }
        if let Some(trail) = &mut self.trail
            && trail.flow == Some(flow)
        {
            trail.reached = Some("internet".to_owned());
        }
        self.leaving.push(Leaving {
            flow,
            packet: ip.to_vec(),
            resolve: false,
        });
        verdict
    }

    /// A UDP answer from outside, for the guest `back` names: from
    /// `back.source`, port `back.source_port`, to the guest's address and
    /// port. Judged as the answer it is, so a rule made since that refuses
    /// it, or the router's internet turned off, stops it here.
    pub fn arrive(&mut self, back: Flow, data: &[u8]) -> (Vec<Out>, Vec<Said>) {
        let mut out = Vec::new();
        let mut said = Vec::new();
        self.counters.received += 1;
        self.costs.clear();
        let Some(to) = self
            .nodes
            .iter()
            .position(|n| n.address == back.destination4())
        else {
            self.counters.refused += 1;
            return (out, said);
        };
        let from_gateway = self.is_router(back.source);
        if !self.live.internet
            || (!from_gateway && self.police(back, &[], &mut said) != Verdict::Allow)
        {
            self.counters.blocked += 1;
            return (out, said);
        }
        if from_gateway {
            self.heard_answer(to, data);
        }
        let datagram = frame::udp(back.source_port, back.destination_port, data);
        let ip = frame::ipv4_packet(back.source4(), back.destination4(), frame::UDP, &datagram);
        let bytes = frame::ethernet(self.nodes[to].mac, GATEWAY_MAC, frame::ETHER_IPV4, &ip);
        self.send_down(to, bytes, &mut out, &mut said);
        (out, said)
    }

    /// A whole IPv4 packet from outside -- TCP, from the router's own
    /// stack -- for the guest it is addressed to. Judged as an answer, like
    /// a UDP one: a conversation the guest began, under rules as they are now.
    pub fn arrive_packet(&mut self, ip: &[u8]) -> (Vec<Out>, Vec<Said>) {
        let mut said = Vec::new();
        self.counters.received += 1;
        self.costs.clear();
        let probe = frame::ethernet(GATEWAY_MAC, GATEWAY_MAC, frame::ETHER_IPV4, ip);
        let Some(packet) = frame::ipv4(&probe) else {
            self.counters.refused += 1;
            return (Vec::new(), said);
        };
        let back = packet.flow;
        let Some(to) = self
            .nodes
            .iter()
            .position(|n| n.address == back.destination)
        else {
            self.counters.refused += 1;
            return (Vec::new(), said);
        };
        if !self.live.internet || self.police(back, ip, &mut said) != Verdict::Allow {
            self.counters.blocked += 1;
            return (Vec::new(), said);
        }
        let bytes = frame::ethernet(self.nodes[to].mac, GATEWAY_MAC, frame::ETHER_IPV4, ip);
        let mut out = Vec::new();
        self.send_down(to, bytes, &mut out, &mut said);
        (out, said)
    }

    /// The address a resolver gave for a name a guest asked, written down
    /// beside the names the network answers itself.
    fn heard_answer(&mut self, to: usize, answer: &[u8]) {
        let Some((name, address)) = frame::dns_first_a(answer) else {
            return;
        };
        let node = self.nodes[to].name.clone();
        if let Some(lookup) = self.lookups.get_mut(&(node, name)) {
            lookup.answer = Some(address);
        }
    }
}

/// The IP packet in an Ethernet frame, alone: a short frame is padded to
/// 60 bytes.
fn ip_alone(bytes: &[u8]) -> &[u8] {
    let ip = bytes.get(14..).unwrap_or_default();
    let length = ip
        .get(2..4)
        .map_or(ip.len(), |l| usize::from(u16::from_be_bytes([l[0], l[1]])));
    &ip[..length.min(ip.len())]
}

/// The ICMP type of an IPv4 packet that carries ICMP.
/// Whether a TCP packet is a reset: "nobody here", from the machine itself.
fn tcp_reset(ip: &[u8]) -> bool {
    transport(ip, frame::TCP)
        .and_then(|segment| segment.get(13))
        .is_some_and(|flags| flags & 0x04 != 0)
}

/// The ICMP type of an IPv4 packet that carries ICMP, or the ICMPv6 type of
/// an IPv6 packet that carries ICMPv6.
fn icmp_type(ip: &[u8]) -> Option<u8> {
    transport(ip, frame::ICMP)
        .or_else(|| transport(ip, frame::ICMPV6))?
        .first()
        .copied()
}

/// The name a TCP packet's data says it is for: the server name in a TLS
/// ClientHello, or the host of an HTTP request.
fn said_name(ip: &[u8]) -> Option<String> {
    let segment = transport(ip, frame::TCP)?;
    let data = segment.get(usize::from(*segment.get(12)? >> 4) * 4..)?;
    frame::tls_server_name(data).or_else(|| frame::http_host(data))
}

/// What an IPv4 or IPv6 packet carries past its header, when it is
/// `protocol`. ponytail: IPv6 extension headers are not walked.
fn transport(ip: &[u8], protocol: u8) -> Option<&[u8]> {
    match ip.first()? >> 4 {
        4 if ip.get(9) == Some(&protocol) => ip.get(usize::from(ip[0] & 0x0f) * 4..),
        6 if ip.get(6) == Some(&protocol) => ip.get(40..),
        _ => None,
    }
}

/// `fd00::/8`, or a bare IPv6 address as a /128.
pub fn parse_cidr6(text: &str) -> Option<(Ipv6Addr, u8)> {
    match text.split_once('/') {
        Some((address, prefix)) => {
            let prefix: u8 = prefix.parse().ok().filter(|p| *p <= 128)?;
            Some((address.parse().ok()?, prefix))
        }
        None => Some((text.parse().ok()?, 128)),
    }
}

/// `10.0.0.0/8`, or a bare address as a /32.
pub fn parse_cidr(text: &str) -> Option<(Ipv4Addr, u8)> {
    match text.split_once('/') {
        Some((address, prefix)) => {
            let prefix: u8 = prefix.parse().ok().filter(|p| *p <= 32)?;
            Some((address.parse().ok()?, prefix))
        }
        None => Some((text.parse().ok()?, 32)),
    }
}

/// A frame waiting for its moment. Only the switch's thread needs one, and
/// the thread is Unix's (`running`); the tests hold the shaper on every system.
#[cfg(any(unix, test))]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Pending {
    at: Instant,
    sequence: u64,
    to: PathBuf,
    bytes: Vec<u8>,
}

/// When each frame goes: latency and jitter on top of the cable's own time,
/// lost at random, dropped past a second of queue (a queue is memory).
///
/// Jitter changes the delay, never the order: TCP reads a reordered frame
/// as lost, and big downloads stall behind resends.
#[cfg(any(unix, test))]
struct Shaper {
    queue: std::collections::BinaryHeap<Reverse<Pending>>,
    busy: HashMap<PathBuf, Instant>,
    /// When the last frame to each port goes; none goes before it.
    last: HashMap<PathBuf, Instant>,
    sequence: u64,
    seed: u64,
}

#[cfg(any(unix, test))]
impl Shaper {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
            | 1;
        Self {
            queue: std::collections::BinaryHeap::new(),
            busy: HashMap::new(),
            last: HashMap::new(),
            sequence: 0,
            seed,
        }
    }

    /// 0 to 1. xorshift: this decides which frames a lab loses, not keys.
    fn random(&mut self) -> f64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Queue it; `false` if it was lost.
    fn schedule(&mut self, traffic: Traffic, to: PathBuf, bytes: Vec<u8>) -> bool {
        if traffic.loss_percent > 0.0 && self.random() * 100.0 < traffic.loss_percent {
            return false;
        }
        let now = Instant::now();
        let mut at = now;
        if traffic.bandwidth_kbps > 0.0 {
            let free = self.busy.get(&to).copied().unwrap_or(now).max(now);
            if free - now > Duration::from_secs(1) {
                return false;
            }
            at = free
                + Duration::from_secs_f64(
                    bytes.len() as f64 * 8.0 / (traffic.bandwidth_kbps * 1000.0),
                );
            self.busy.insert(to.clone(), at);
        }
        let jitter = traffic.jitter_ms * (self.random() * 2.0 - 1.0);
        at += Duration::from_secs_f64((traffic.latency_ms + jitter).max(0.0) / 1000.0);
        if let Some(before) = self.last.get(&to) {
            at = at.max(*before);
        }
        self.last.insert(to.clone(), at);
        self.sequence += 1;
        self.queue.push(Reverse(Pending {
            at,
            sequence: self.sequence,
            to,
            bytes,
        }));
        true
    }

    /// Everything due, and how long until the next one.
    fn due(&mut self) -> (Vec<Pending>, Option<Duration>) {
        let now = Instant::now();
        let mut ready = Vec::new();
        while self.queue.peek().is_some_and(|Reverse(p)| p.at <= now) {
            ready.push(self.queue.pop().expect("peeked").0);
        }
        let wait = self.queue.peek().map(|Reverse(p)| p.at - now);
        (ready, wait)
    }
}

#[cfg(unix)]
pub use running::Switch;

#[cfg(unix)]
mod running {
    use std::collections::HashMap;
    use std::io::ErrorKind;
    use std::os::unix::net::UnixDatagram;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use beatbox_engine::Emitter;

    use super::{Leaving, Net, Shaper, Traffic};
    use crate::router::{self, At, Back};

    /// The thread that carries frames, while it runs, and the uplink beside
    /// it that carries what leaves through the router.
    pub struct Switch {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
        uplink: Option<JoinHandle<()>>,
        uplink_path: PathBuf,
    }

    impl Switch {
        /// Listen on `path` and carry frames for `net` until [`Switch::stop`].
        pub fn start(path: &Path, net: Arc<Mutex<Net>>, emit: Emitter) -> std::io::Result<Self> {
            let _ = std::fs::remove_file(path);
            let socket = UnixDatagram::bind(path)?;
            let uplink_path = path.with_file_name("uplink.sock");
            let _ = std::fs::remove_file(&uplink_path);
            let back = UnixDatagram::bind(&uplink_path)?;
            let (leave, leaving) = mpsc::channel();
            let switch_path = path.to_owned();
            let at = {
                let net = net.lock().unwrap_or_else(PoisonError::into_inner);
                At {
                    gateway: net.live.gateway(),
                    prefix: net.live.prefix,
                }
            };
            let uplink = std::thread::Builder::new()
                .name(format!("beatbox-uplink-{}", emit.sandbox_id()))
                .spawn(move || {
                    router::carry_out(&leaving, &back, &switch_path, router::resolver(), at)
                })?;
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = Arc::clone(&stop);
            let from_uplink = uplink_path.clone();
            let thread = std::thread::Builder::new()
                .name(format!("beatbox-switch-{}", emit.sandbox_id()))
                .spawn(move || carry(&socket, &net, &emit, &stopping, &leave, &from_uplink))?;
            Ok(Self {
                path: path.to_owned(),
                stop,
                thread: Some(thread),
                uplink: Some(uplink),
                uplink_path,
            })
        }

        /// Whether the thread is still carrying.
        pub fn alive(&self) -> bool {
            self.thread.as_ref().is_some_and(|t| !t.is_finished())
        }

        /// Stop carrying, and take the socket away. An empty datagram wakes
        /// the thread, so an idle switch never has to wake up on a timer to
        /// find out it should stop.
        pub fn stop(mut self) {
            self.halt();
        }

        fn halt(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Ok(waker) = UnixDatagram::unbound() {
                let _ = waker.send_to(&[], &self.path);
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            // The switch's thread held the only sender: gone, the uplink ends.
            if let Some(uplink) = self.uplink.take() {
                let _ = uplink.join();
            }
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(&self.uplink_path);
        }
    }

    impl Drop for Switch {
        fn drop(&mut self) {
            if self.thread.is_some() {
                self.halt();
            }
        }
    }

    fn carry(
        socket: &UnixDatagram,
        net: &Mutex<Net>,
        emit: &Emitter,
        stop: &AtomicBool,
        leave: &Sender<Leaving>,
        uplink: &Path,
    ) {
        let mut shaper = Shaper::new();
        let mut cables = HashMap::new();
        let mut buffer = vec![0u8; 65_536];
        // The first a few seconds in, then every half a minute.
        let mut announced = Instant::now()
            .checked_sub(super::ANNOUNCE_EVERY - Duration::from_secs(3))
            .unwrap_or_else(Instant::now);
        loop {
            // Also after a wait that ran out: the waker may never come.
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let next = super::ANNOUNCE_EVERY.saturating_sub(announced.elapsed());
            if next.is_zero() {
                announced = Instant::now();
                let mut net = net.lock().unwrap_or_else(PoisonError::into_inner);
                let outs: Vec<_> = net
                    .announce()
                    .into_iter()
                    .map(|(to, bytes)| (net.nodes[to].port.clone(), bytes))
                    .collect();
                let traffic = net.live.traffic;
                drop(net);
                for (to, bytes) in outs {
                    shaper.schedule(traffic, to, bytes);
                }
            }
            let (ready, wait) = shaper.due();
            for frame in ready {
                send(&mut cables, &frame.to, &frame.bytes);
            }
            let wait = wait.map_or(next, |w| w.min(next));
            let _ = socket.set_read_timeout(Some(wait.max(Duration::from_millis(1))));
            let (size, from) = match socket.recv_from(&mut buffer) {
                Ok(got) => got,
                // A signal to the daemon interrupts the wait: nothing wrong.
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(error) => {
                    log::warn!("a network switch could not read: {error}");
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let Some(path) = from.as_pathname() else {
                continue;
            };
            let (outs, said) = {
                let mut net = net.lock().unwrap_or_else(PoisonError::into_inner);
                let (outs, said) = if path == uplink {
                    match router::decode(&buffer[..size]) {
                        Some(Back::Udp(back, data)) => net.arrive(back, data),
                        Some(Back::Packet(ip)) => net.arrive_packet(ip),
                        None => continue,
                    }
                } else {
                    let Some(index) = net.port_of(path) else {
                        continue;
                    };
                    net.handle(index, &buffer[..size])
                };
                for each in net.take_leaving() {
                    let _ = leave.send(each);
                }
                let costs = net.take_costs();
                let outs: Vec<(PathBuf, Vec<u8>, Traffic)> = outs
                    .into_iter()
                    .enumerate()
                    .map(|(i, (to, bytes))| {
                        let cost = costs.get(i).copied().unwrap_or(net.live.traffic);
                        (net.nodes[to].port.clone(), bytes, cost)
                    })
                    .collect();
                (outs, said)
            };
            let (mut delivered, mut lost) = (0u64, 0u64);
            for (to, bytes, traffic) in outs {
                if shaper.schedule(traffic, to, bytes) {
                    delivered += 1;
                } else {
                    lost += 1;
                }
            }
            {
                let mut net = net.lock().unwrap_or_else(PoisonError::into_inner);
                net.counters.delivered += delivered;
                net.counters.lost += lost;
            }
            for (name, data) in said {
                if let Err(error) = emit.emit(name, data) {
                    log::warn!("a network switch could not say {name}: {error}");
                }
            }
        }
    }

    /// One frame down the cable at `to`, from that cable's own socket.
    ///
    /// A paused VM stops reading, and what waits for it stays charged to the
    /// socket that sent it: on one socket for every port, about 160 frames
    /// for a paused machine fill the send buffer and stall every port.
    /// Never blocking, a full cable drops the frame, as a real port would. Guests read with `recv`, so the sender needs no name.
    fn send(cables: &mut HashMap<PathBuf, UnixDatagram>, to: &Path, bytes: &[u8]) {
        if !cables.contains_key(to) {
            let Ok(socket) = UnixDatagram::unbound() else {
                return;
            };
            let _ = socket.set_nonblocking(true);
            cables.insert(to.to_owned(), socket);
        }
        match cables[to].send_to(bytes, to) {
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            // Nobody listening -- a guest not started yet, or gone -- is an
            // unplugged cable. Its socket goes too, with what it still held.
            Err(_) => {
                cables.remove(to);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net() -> Net {
        let mut net = Net::new(Baseline {
            network: Ipv4Addr::new(10, 10, 10, 0),
            prefix: 24,
            default_allow: true,
            rules: Vec::new(),
            answers: true,
            groups: BTreeMap::new(),
            services: BTreeMap::new(),
            dns: BTreeMap::new(),
            records: Vec::new(),
            domain: "local".into(),
            traffic: Traffic::default(),
            internet: false,
            names: Vec::new(),
        });
        for (name, last) in [("a", 10), ("b", 20)] {
            let address = Ipv4Addr::new(10, 10, 10, last);
            net.nodes.push(Node {
                name: name.into(),
                address,
                mac: frame::node_mac(address),
                port: PathBuf::from(name),
                at: End {
                    device: 0,
                    port: u16::from(last),
                },
            });
        }
        net
    }

    fn ping(net: &Net, from: usize, to: usize, id: u16) -> Vec<u8> {
        let (a, b) = (&net.nodes[from], &net.nodes[to]);
        let icmp = [8, 0, 0, 0, (id >> 8) as u8, id as u8, 0, 1];
        let ip = frame::ipv4_packet(a.address, b.address, frame::ICMP, &icmp);
        frame::ethernet(b.mac, a.mac, frame::ETHER_IPV4, &ip)
    }

    fn reply(net: &Net, from: usize, to: usize, id: u16) -> Vec<u8> {
        let mut bytes = ping(net, from, to, id);
        bytes[34] = 0;
        bytes
    }

    /// UDP from node `from` to `to`:`port`, through the router (its MAC).
    fn outward(net: &Net, from: usize, to: Ipv4Addr, port: u16, data: &[u8]) -> Vec<u8> {
        let a = &net.nodes[from];
        let ip = frame::ipv4_packet(a.address, to, frame::UDP, &frame::udp(40000, port, data));
        frame::ethernet(GATEWAY_MAC, a.mac, frame::ETHER_IPV4, &ip)
    }

    fn query(name: &str) -> Vec<u8> {
        let mut query = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            query.push(label.len() as u8);
            query.extend_from_slice(label.as_bytes());
        }
        query.extend_from_slice(&[0, 0, 1, 0, 1]);
        query
    }

    const OUT_THERE: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

    #[test]
    fn with_the_internet_off_nothing_leaves() {
        let mut net = net();
        let (out, _) = net.handle(0, &outward(&net, 0, OUT_THERE, 53, b"hi"));
        assert!(out.is_empty());
        assert!(net.take_leaving().is_empty());
        assert_eq!(net.counters.unroutable, 1);
    }

    #[test]
    fn with_the_internet_on_udp_leaves_as_the_guest_sent_it() {
        let mut net = net();
        net.set_internet(true);
        let (_, said) = net.handle(0, &outward(&net, 0, OUT_THERE, 123, b"time?"));
        let leaving = net.take_leaving();
        assert_eq!(leaving.len(), 1);
        assert_eq!(leaving[0].flow.source, net.nodes[0].address);
        assert_eq!(leaving[0].flow.destination, OUT_THERE);
        assert_eq!(leaving[0].flow.destination_port, 123);
        assert_eq!(leaving[0].data(), b"time?");
        assert!(!leaving[0].resolve);
        assert_eq!(said[0].0, "connection.created");
    }

    #[test]
    fn this_computer_is_never_reached_through_the_router() {
        let mut net = net();
        net.set_internet(true);
        for to in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(169, 254, 1, 1),
            Ipv4Addr::new(224, 0, 0, 251),
            Ipv4Addr::BROADCAST,
            Ipv4Addr::new(0, 1, 2, 3),
        ] {
            net.handle(0, &outward(&net, 0, to, 53, b"x"));
        }
        assert!(net.take_leaving().is_empty());
        assert_eq!(net.counters.unroutable, 5);
    }

    #[test]
    fn a_rule_against_the_internet_stops_it() {
        let mut net = net();
        net.set_internet(true);
        net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Deny,
            source: "a".into(),
            destination: "internet".into(),
            protocol: None,
            ports: None,
            ..Rule::default()
        });
        let (_, said) = net.handle(0, &outward(&net, 0, OUT_THERE, 123, b"x"));
        assert!(net.take_leaving().is_empty());
        assert_eq!(said[0].0, "packet.blocked");
        // b is not in the rule: it goes.
        net.handle(1, &outward(&net, 1, OUT_THERE, 123, b"x"));
        assert_eq!(net.take_leaving().len(), 1);
        // "internet" is never a node of this network.
        assert!(!net.matches("internet", net.nodes[1].address.into()));
    }

    #[test]
    fn an_answer_comes_back_to_the_guest_that_asked_even_when_nothing_else_may() {
        let mut net = net();
        net.set_internet(true);
        net.live.default_allow = false;
        net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Allow,
            source: "a".into(),
            destination: "internet".into(),
            protocol: Some(frame::UDP),
            ports: Some((123, 123)),
            ..Rule::default()
        });
        net.handle(0, &outward(&net, 0, OUT_THERE, 123, b"time?"));
        let leaving = net.take_leaving().remove(0);
        let (out, _) = net.arrive(leaving.flow.reverse(), b"noon");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 0);
        let back = frame::ipv4(&out[0].1).unwrap();
        assert_eq!(back.flow.source, OUT_THERE);
        assert_eq!(back.flow.source_port, 123);
        assert_eq!(back.flow.destination_port, 40000);
        assert_eq!(frame::udp_data(back.payload), b"noon");
        // Something from out there nobody asked for does not come in.
        let stranger = Flow {
            source: Ipv4Addr::new(8, 8, 8, 8).into(),
            destination: net.nodes[0].address.into(),
            protocol: frame::UDP,
            source_port: 9,
            destination_port: 40000,
        };
        assert!(net.arrive(stranger, b"hello").0.is_empty());
        // And turning the internet off stops what was on its way back.
        net.set_internet(false);
        assert!(net.arrive(leaving.flow.reverse(), b"late").0.is_empty());
    }

    #[test]
    fn a_rule_by_name_hides_points_or_cuts() {
        let mut net = net();
        net.set_internet(true);
        let gateway = net.live.gateway();
        let rule = |action, name: &str, source: &str| NameRule {
            id: String::new(),
            action,
            name: name.into(),
            source: source.into(),
            uses: 0,
        };
        net.add_name(rule(NameAction::Hide, "*.facebook.com", "a"));
        net.add_name(rule(NameAction::Point("b".into()), "intranet.local", "any"));
        let cut = net.add_name(rule(NameAction::Cut, "portal.local", "any"));
        let ask = |net: &mut Net, from: usize, name: &str| {
            let (out, _) = net.handle(from, &outward(net, from, gateway, 53, &query(name)));
            out.first()
                .map(|o| frame::udp_data(frame::ipv4(&o.1).unwrap().payload).to_vec())
        };
        // Hidden for a, even with the internet on: "no such name", at once.
        let answer = ask(&mut net, 0, "www.facebook.com").expect("answered here");
        assert_eq!(answer[3] & 0x0f, 3, "NXDOMAIN");
        assert!(net.take_leaving().is_empty());
        assert!(
            net.lookups()
                .iter()
                .any(|l| l["name"] == "www.facebook.com" && l["rule"] == "n1")
        );
        // Not for b: asked outside, as any name.
        assert!(ask(&mut net, 1, "www.facebook.com").is_none());
        assert_eq!(net.take_leaving().len(), 1);
        // Pointed: the name is b.
        let answer = ask(&mut net, 0, "intranet.local").expect("answered here");
        assert_eq!(&answer[answer.len() - 4..], &net.nodes[1].address.octets());
        // Cut: a page asked of portal.local is answered with a reset.
        let (a, b) = (net.nodes[0].clone(), net.nodes[1].clone());
        let mut segment = vec![
            0x9c, 0x40, 0, 80, 0, 0, 0, 1, 0, 0, 0, 1, 0x50, 0x18, 0xff, 0xff, 0, 0, 0, 0,
        ];
        segment.extend_from_slice(b"GET / HTTP/1.1\r\nHost: portal.local\r\n\r\n");
        let ip = frame::ipv4_packet(a.address, b.address, frame::TCP, &segment);
        let (out, _) = net.handle(0, &frame::ethernet(b.mac, a.mac, frame::ETHER_IPV4, &ip));
        assert!(out.iter().all(|o| o.0 != 1), "b never gets it");
        let reset = frame::ipv4(&out.iter().find(|o| o.0 == 0).expect("a hears").1).unwrap();
        assert_eq!(reset.payload[13] & 0x04, 0x04, "a reset");
        let flows = net.flows();
        let told = flows.iter().find(|f| f["port"] == 80).unwrap();
        assert_eq!(
            (told["action"].as_str(), told["rule"].as_str()),
            (Some("deny"), Some(cut.as_str()))
        );
        assert_eq!(
            net.live.names.iter().map(|n| n.uses).collect::<Vec<_>>(),
            vec![1, 1, 1]
        );
        // What it still sends, with no name in it, stays cut.
        let bare = frame::ipv4_packet(a.address, b.address, frame::TCP, &segment[..20]);
        let (out, _) = net.handle(0, &frame::ethernet(b.mac, a.mac, frame::ETHER_IPV4, &bare));
        assert!(out.iter().all(|o| o.0 != 1));
        assert_eq!(
            net.flows().iter().find(|f| f["port"] == 80).unwrap()["action"],
            "deny"
        );
        // But its reset passes, so the server lets go of the connection.
        let mut reset = segment[..20].to_vec();
        reset[13] = 0x14;
        let ip_reset = frame::ipv4_packet(a.address, b.address, frame::TCP, &reset);
        let (out, _) = net.handle(
            0,
            &frame::ethernet(b.mac, a.mac, frame::ETHER_IPV4, &ip_reset),
        );
        assert!(out.iter().any(|o| o.0 == 1), "the reset reaches b");
        assert_eq!(
            net.flows().iter().find(|f| f["port"] == 80).unwrap()["action"],
            "deny"
        );
        // Without the rule, the same page goes through.
        net.remove_name(&cut);
        let (out, _) = net.handle(0, &frame::ethernet(b.mac, a.mac, frame::ETHER_IPV4, &ip));
        assert!(out.iter().any(|o| o.0 == 1));
    }

    #[test]
    fn a_name_the_network_does_not_know_is_asked_outside_and_answered_by_the_gateway() {
        let mut net = net();
        let gateway = net.live.gateway();
        // Off: the network says it does not know.
        let (out, _) = net.handle(0, &outward(&net, 0, gateway, 53, &query("example.com")));
        assert_eq!(out.len(), 1);
        net.set_internet(true);
        let (out, _) = net.handle(0, &outward(&net, 0, gateway, 53, &query("example.com")));
        assert!(out.is_empty(), "answered later, from outside");
        let leaving = net.take_leaving().remove(0);
        assert!(leaving.resolve);
        // A resolver's answer: the query, flagged as a response, one A record.
        let mut answer = query("example.com");
        answer[2] |= 0x80;
        answer[7] = 1;
        answer.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 216, 34]);
        let (out, _) = net.arrive(leaving.flow.reverse(), &answer);
        let back = frame::ipv4(&out[0].1).unwrap();
        assert_eq!(back.flow.source, gateway);
        assert_eq!(back.flow.source_port, 53);
        let looked = net.lookups();
        let example = looked.iter().find(|l| l["name"] == "example.com").unwrap();
        assert_eq!(example["answer"], "93.184.216.34");
        // A name the network knows is still its own answer.
        net.live
            .dns
            .insert("portal.local".into(), Ipv4Addr::new(10, 10, 10, 20));
        let (out, _) = net.handle(0, &outward(&net, 0, gateway, 53, &query("portal.local")));
        assert_eq!(out.len(), 1);
        assert!(net.take_leaving().is_empty());
    }

    #[test]
    fn tcp_leaves_whole_and_ping_does_not_yet() {
        let mut net = net();
        net.set_internet(true);
        let a = net.nodes[0].clone();
        let syn = [
            0x9c, 0x40, 0, 80, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0, 0,
        ];
        let ip = frame::ipv4_packet(a.address, OUT_THERE, frame::TCP, &syn);
        let bytes = frame::ethernet(GATEWAY_MAC, a.mac, frame::ETHER_IPV4, &ip);
        net.handle(0, &bytes);
        let leaving = net.take_leaving();
        assert_eq!(leaving.len(), 1);
        assert_eq!(
            leaving[0].packet, ip,
            "the packet as sent, for the router's own TCP"
        );
        assert_eq!(leaving[0].tcp_flags(), 0x02);
        let echo = frame::ipv4_packet(a.address, OUT_THERE, frame::ICMP, &[8, 0, 0, 0, 0, 1, 0, 1]);
        net.handle(
            0,
            &frame::ethernet(GATEWAY_MAC, a.mac, frame::ETHER_IPV4, &echo),
        );
        assert!(net.take_leaving().is_empty());
        assert_eq!(net.counters.unroutable, 1);
    }

    #[test]
    fn a_packet_from_the_router_reaches_its_guest_as_an_answer() {
        let mut net = net();
        net.set_internet(true);
        net.live.default_allow = false;
        net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Allow,
            source: "a".into(),
            destination: "internet".into(),
            protocol: Some(frame::TCP),
            ports: Some((80, 80)),
            ..Rule::default()
        });
        let a = net.nodes[0].clone();
        let syn = [
            0x9c, 0x40, 0, 80, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0, 0,
        ];
        let out = frame::ipv4_packet(a.address, OUT_THERE, frame::TCP, &syn);
        net.handle(
            0,
            &frame::ethernet(GATEWAY_MAC, a.mac, frame::ETHER_IPV4, &out),
        );
        let synack = [
            0, 80, 0x9c, 0x40, 0, 0, 0, 9, 0, 0, 0, 2, 0x50, 0x12, 0xff, 0xff, 0, 0, 0, 0,
        ];
        let back = frame::ipv4_packet(OUT_THERE, net.nodes[0].address, frame::TCP, &synack);
        let (delivered, _) = net.arrive_packet(&back);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].0, 0);
        assert_eq!(&delivered[0].1[14..], &back[..]);
        // To a guest that never asked: no.
        let stranger = frame::ipv4_packet(OUT_THERE, net.nodes[1].address, frame::TCP, &synack);
        assert!(net.arrive_packet(&stranger).0.is_empty());
    }

    #[test]
    fn a_ping_goes_across_and_is_news_once() {
        let mut net = net();
        let (out, said) = net.handle(0, &ping(&net, 0, 1, 7));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 1);
        assert_eq!(said[0].0, "connection.created");
        assert_eq!(said[0].1["destination_node"], "b");
        let (_, said) = net.handle(0, &ping(&net, 0, 1, 7));
        assert!(said.is_empty());
        // The reply is part of the same conversation, not a new one.
        let (out, said) = net.handle(1, &reply(&net, 1, 0, 7));
        assert_eq!(out.len(), 1);
        assert!(said.is_empty());
        // And the flows say which is which, so a page can put them together.
        let flows = net.flows();
        let from_b = flows.iter().find(|f| f["source_node"] == "b").unwrap();
        let from_a = flows.iter().find(|f| f["source_node"] == "a").unwrap();
        assert_eq!(from_b["answer"], true);
        assert_eq!(from_a["answer"], false);
        assert_eq!(from_a["source_port"], 7);
    }

    #[test]
    fn a_conversation_knows_whether_the_machine_answered_or_said_no() {
        let mut net = net();
        net.handle(0, &ping(&net, 0, 1, 7));
        let asked = |net: &Net, port: u64| {
            net.flows()
                .into_iter()
                .find(|f| f["source_node"] == "a" && f["source_port"] == port)
                .unwrap()
        };
        assert_eq!(asked(&net, 7)["replied"], false);
        net.handle(1, &reply(&net, 1, 0, 7));
        assert_eq!(asked(&net, 7)["replied"], true);
        assert_eq!(asked(&net, 7)["reset"], false);
        // A knock on a closed port: the machine itself answers with a reset.
        let (a, b) = (net.nodes[0].clone(), net.nodes[1].clone());
        let syn = [
            0x9c, 0x40, 0, 22, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0, 0,
        ];
        let rst = [
            0, 22, 0x9c, 0x40, 0, 0, 0, 0, 0, 0, 0, 2, 0x50, 0x14, 0, 0, 0, 0, 0, 0,
        ];
        let ip = frame::ipv4_packet(a.address, b.address, frame::TCP, &syn);
        net.handle(0, &frame::ethernet(b.mac, a.mac, frame::ETHER_IPV4, &ip));
        let ip = frame::ipv4_packet(b.address, a.address, frame::TCP, &rst);
        net.handle(1, &frame::ethernet(a.mac, b.mac, frame::ETHER_IPV4, &ip));
        assert_eq!(asked(&net, 0x9c40)["replied"], true);
        assert_eq!(asked(&net, 0x9c40)["reset"], true);
    }

    #[test]
    fn a_flow_says_which_rule_decided_and_an_answer_says_none() {
        let mut net = net();
        net.live.default_allow = false;
        let id = net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Allow,
            source: "a".into(),
            destination: "b".into(),
            protocol: Some(frame::ICMP),
            ports: None,
            ..Rule::default()
        });
        net.handle(0, &ping(&net, 0, 1, 7));
        net.handle(1, &reply(&net, 1, 0, 7));
        let flows = net.flows();
        let request = flows.iter().find(|f| f["source_node"] == "a").unwrap();
        let answer = flows.iter().find(|f| f["source_node"] == "b").unwrap();
        assert_eq!(request["rule"], id.as_str());
        assert_eq!(answer["rule"], serde_json::Value::Null);
        assert_eq!(answer["action"], "allow");
    }

    #[test]
    fn a_deny_rule_stops_it_and_removing_it_lets_it_through() {
        let mut net = net();
        net.handle(0, &ping(&net, 0, 1, 7));
        let id = net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Deny,
            source: "a".into(),
            destination: "b".into(),
            protocol: Some(frame::ICMP),
            ports: None,
            ..Rule::default()
        });
        let (out, said) = net.handle(0, &ping(&net, 0, 1, 7));
        assert!(out.is_empty());
        assert_eq!(said[0].0, "packet.blocked");
        assert_eq!(said[0].1["rule"], id.as_str());
        assert_eq!(net.counters.blocked, 1);
        // B may still ping A, and A's replies to that are established.
        net.handle(1, &ping(&net, 1, 0, 9));
        let (out, _) = net.handle(0, &reply(&net, 0, 1, 9));
        assert_eq!(out.len(), 1);
        assert!(net.remove_rule(&id));
        let (out, said) = net.handle(0, &ping(&net, 0, 1, 7));
        assert_eq!(out.len(), 1);
        assert_eq!(said[0].0, "connection.created");
    }

    #[test]
    fn default_deny_lets_answers_to_allowed_conversations_back() {
        let mut net = net();
        net.live.default_allow = false;
        let (out, _) = net.handle(0, &ping(&net, 0, 1, 7));
        assert!(out.is_empty());
        net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Allow,
            source: "10.10.10.10".into(),
            destination: "10.10.10.0/24".into(),
            protocol: None,
            ports: None,
            ..Rule::default()
        });
        assert_eq!(net.handle(0, &ping(&net, 0, 1, 7)).0.len(), 1);
        assert_eq!(net.handle(1, &reply(&net, 1, 0, 7)).0.len(), 1);
        // But B cannot start one of its own.
        assert!(net.handle(1, &ping(&net, 1, 0, 8)).0.is_empty());
    }

    /// A TCP SYN from node `from` to `to`:`port`.
    fn syn(net: &Net, from: usize, to: usize, port: u16) -> Vec<u8> {
        let (a, b) = (&net.nodes[from], &net.nodes[to]);
        let [high, low] = port.to_be_bytes();
        let tcp = [
            0x9c, 0x40, high, low, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0, 0,
        ];
        let ip = frame::ipv4_packet(a.address, b.address, frame::TCP, &tcp);
        frame::ethernet(b.mac, a.mac, frame::ETHER_IPV4, &ip)
    }

    #[test]
    fn a_limit_lets_so_many_new_conversations_through_and_cuts_the_rest() {
        let mut net = net();
        net.live.default_allow = false;
        net.add_rule(Rule {
            verdict: Verdict::Allow,
            source: "a".into(),
            destination: "b".into(),
            protocol: Some(frame::TCP),
            ports: Some((22, 22)),
            limit: Some(Limit {
                count: 3,
                per_ms: 60_000,
                each: true,
                over: Verdict::Reject,
            }),
            ..Rule::default()
        });
        let attempt = |net: &Net, from_port: u16| {
            let mut bytes = syn(net, 0, 1, 22);
            bytes[34..36].copy_from_slice(&from_port.to_be_bytes());
            bytes
        };
        let mut through = 0;
        let mut refused = 0;
        for i in 0..5 {
            let (out, _) = net.handle(0, &attempt(&net, 40000 + i));
            if out.iter().any(|o| o.0 == 1) {
                through += 1;
            } else if out.iter().any(|o| o.0 == 0) {
                refused += 1;
            }
        }
        assert_eq!((through, refused), (3, 2));
        // The same conversation tried again stays cut, and says why.
        let (out, _) = net.handle(0, &attempt(&net, 40004));
        assert!(out.iter().all(|o| o.0 != 1));
        let flows = net.flows();
        assert_eq!(flows.iter().filter(|f| f["limited"] == true).count(), 2);
        let cut = flows.iter().find(|f| f["limited"] == true).unwrap();
        assert_eq!(cut["path"]["limited"], true);
        assert_eq!(cut["path"]["verdict"], "reject");
        // A packet of a conversation already let through is not counted again.
        let (out, _) = net.handle(0, &attempt(&net, 40000));
        assert!(out.iter().any(|o| o.0 == 1));
    }

    #[test]
    fn a_reject_rule_answers_at_once_and_a_range_is_every_port_in_it() {
        let mut net = net();
        net.add_rule(Rule {
            verdict: Verdict::Reject,
            source: "a".into(),
            destination: "b".into(),
            protocol: Some(frame::TCP),
            ports: Some((8000, 8100)),
            ..Rule::default()
        });
        let (out, said) = net.handle(0, &syn(&net, 0, 1, 8080));
        assert_eq!(said[0].1["action"], "reject");
        // Back to A, from B's address: port unreachable, about A's own packet.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 0);
        let answer = frame::ipv4(&out[0].1).unwrap();
        assert_eq!(answer.flow.source, net.nodes[1].address);
        assert_eq!(answer.flow.destination, net.nodes[0].address);
        assert_eq!(answer.flow.protocol, frame::ICMP);
        assert_eq!(&answer.payload[..2], &[frame::ICMP_UNREACHABLE, 3]);
        // Outside the range, the default.
        let (out, _) = net.handle(0, &syn(&net, 0, 1, 9000));
        assert_eq!(out[0].0, 1);
    }

    #[test]
    fn a_group_is_its_machines_and_answers_can_be_judged_too() {
        let mut net = net();
        net.live.default_allow = false;
        net.set_group("Servers", vec!["b".into()]).unwrap();
        net.add_rule(Rule {
            verdict: Verdict::Allow,
            source: "a".into(),
            destination: "group:Servers".into(),
            ..Rule::default()
        });
        assert_eq!(net.handle(0, &ping(&net, 0, 1, 7)).0.len(), 1);
        assert_eq!(net.handle(1, &reply(&net, 1, 0, 7)).0.len(), 1);
        // With answers judged like anything else, B's reply needs a rule.
        net.set_answers(false);
        net.handle(0, &ping(&net, 0, 1, 7));
        assert!(net.handle(1, &reply(&net, 1, 0, 7)).0.is_empty());
        // A group a rule names stays.
        assert!(net.set_group("Servers", Vec::new()).is_err());
    }

    #[test]
    fn a_rule_for_new_conversations_leaves_answers_alone() {
        let mut net = net();
        net.set_answers(false);
        net.add_rule(Rule {
            verdict: Verdict::Deny,
            only_new: true,
            source: "b".into(),
            destination: "a".into(),
            ..Rule::default()
        });
        net.handle(0, &ping(&net, 0, 1, 7));
        assert_eq!(net.handle(1, &reply(&net, 1, 0, 7)).0.len(), 1);
        assert!(net.handle(1, &ping(&net, 1, 0, 9)).0.is_empty());
    }

    #[test]
    fn an_icmp_type_and_the_order() {
        let mut net = net();
        let ping_rule = net.add_rule(Rule {
            verdict: Verdict::Deny,
            source: "a".into(),
            destination: "b".into(),
            protocol: Some(frame::ICMP),
            icmp_type: Some(8),
            ..Rule::default()
        });
        assert!(net.handle(0, &ping(&net, 0, 1, 7)).0.is_empty());
        // An echo reply is another type.
        assert_eq!(net.handle(0, &reply(&net, 0, 1, 5)).0.len(), 1);
        let flows = net.flows();
        let told = flows
            .iter()
            // The echo request is the one with identifier 7; the reply
            // below is another conversation from the same machine, and
            // the flows come out of a hash map in no fixed order.
            .find(|f| f["protocol"] == "icmp" && f["source_node"] == "a" && f["source_port"] == 7)
            .unwrap();
        assert_eq!(
            (told["rule"].as_str(), told["action"].as_str()),
            (Some(ping_rule.as_str()), Some("deny"))
        );
        // Put first, an allow decides before it.
        let allow = net.add_rule(Rule {
            verdict: Verdict::Allow,
            source: "a".into(),
            ..Rule::default()
        });
        assert!(net.move_rule(&allow, 0));
        assert_eq!(net.live.rules[0].id, allow);
        assert_eq!(net.handle(0, &ping(&net, 0, 1, 8)).0.len(), 1);
    }

    #[test]
    fn a_frame_that_is_not_ip_is_judged_only_by_its_ethertype() {
        let mut net = net();
        let lldp = frame::ethernet(BROADCAST, net.nodes[0].mac, 0x88cc, &[0; 46]);
        let other = frame::ethernet(BROADCAST, net.nodes[0].mac, 0x88b5, &[0; 46]);
        net.add_rule(Rule {
            verdict: Verdict::Deny,
            ..Rule::default()
        });
        // A rule for IP does not touch it.
        assert_eq!(net.handle(0, &lldp).0.len(), 1);
        net.live.rules.clear();
        net.add_rule(Rule {
            verdict: Verdict::Deny,
            ethertype: Some(0x88cc),
            ..Rule::default()
        });
        assert!(net.handle(0, &lldp).0.is_empty());
        assert_eq!(net.handle(0, &other).0.len(), 1);
        // Nor does one for a frame touch IP.
        assert_eq!(net.handle(0, &ping(&net, 0, 1, 7)).0.len(), 1);
    }

    #[test]
    fn nobody_sends_as_somebody_else() {
        let mut net = net();
        let mut forged = ping(&net, 0, 1, 7);
        forged[29] = 99; // source address 10.10.10.99
        assert!(net.handle(0, &forged).0.is_empty());
        let mut forged = ping(&net, 0, 1, 7);
        forged[11] = 0x55; // another MAC
        assert!(net.handle(0, &forged).0.is_empty());
        // To B's MAC, but C's address.
        let mut sideways = ping(&net, 0, 1, 7);
        sideways[33] = 30;
        assert!(net.handle(0, &sideways).0.is_empty());
        assert_eq!(net.counters.refused, 3);
        // Another protocol from someone else's MAC is refused all the same.
        let mut six = ping(&net, 0, 1, 7);
        six[12..14].copy_from_slice(&[0x86, 0xdd]);
        six[11] = 0x55;
        assert!(net.handle(0, &six).0.is_empty());
        assert_eq!(net.counters.refused, 4);
    }

    /// a on the router; b and c on a switch cabled to the router; d on a
    /// second switch behind the first.
    fn branch() -> Net {
        let mut net = net();
        net.nodes.truncate(1);
        let switch = net.add_device(Kind::Switch, "sw1");
        let further = net.add_device(Kind::Switch, "sw2");
        net.link(
            End {
                device: 0,
                port: 100,
            },
            End {
                device: switch,
                port: 0,
            },
        )
        .unwrap();
        net.link(
            End {
                device: switch,
                port: 7,
            },
            End {
                device: further,
                port: 0,
            },
        )
        .unwrap();
        for (name, last, at) in [
            (
                "b",
                20,
                End {
                    device: switch,
                    port: 1,
                },
            ),
            (
                "c",
                30,
                End {
                    device: switch,
                    port: 2,
                },
            ),
            (
                "d",
                40,
                End {
                    device: further,
                    port: 1,
                },
            ),
        ] {
            let address = Ipv4Addr::new(10, 10, 10, last);
            net.nodes.push(Node {
                name: name.into(),
                address,
                mac: frame::node_mac(address),
                port: PathBuf::from(name),
                at,
            });
        }
        net
    }

    #[test]
    fn machines_on_one_switch_talk_without_the_router() {
        let mut net = branch();
        net.set_default(false);
        // b to c: the switch carries it; no rule is asked, nothing is counted.
        let (out, said) = net.handle(1, &ping(&net, 1, 2, 7));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 2);
        assert!(said.is_empty());
        assert!(net.flows().is_empty(), "the router never saw it");
        // b to d, across two switches: the same.
        let (out, _) = net.handle(1, &ping(&net, 1, 3, 7));
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn what_crosses_the_router_is_judged_by_the_firewall() {
        let mut net = branch();
        net.set_default(false);
        // b (on the switch) to a (on the router): through the router, refused.
        assert!(net.handle(1, &ping(&net, 1, 0, 7)).0.is_empty());
        assert_eq!(net.counters.blocked, 1);
        net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Allow,
            source: "b".into(),
            destination: "a".into(),
            protocol: Some(frame::ICMP),
            ports: None,
            ..Rule::default()
        });
        let (out, _) = net.handle(1, &ping(&net, 1, 0, 8));
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), [0]);
        // And the answer goes back down through the switch.
        let (out, _) = net.handle(0, &reply(&net, 0, 1, 8));
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn the_gateway_answers_a_machine_behind_a_switch() {
        let mut net = branch();
        // Ping the gateway from d, two switches away.
        let d = net.nodes[3].clone();
        let icmp = [8, 0, 0, 0, 0, 9, 0, 1];
        let ip = frame::ipv4_packet(d.address, net.live.gateway(), frame::ICMP, &icmp);
        let (out, _) = net.handle(
            3,
            &frame::ethernet(GATEWAY_MAC, d.mac, frame::ETHER_IPV4, &ip),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 3);
        assert_eq!(
            frame::ipv4(&out[0].1).unwrap().payload[0],
            0,
            "an echo reply"
        );
        // An answer from outside reaches it too.
        net.set_internet(true);
        let back = Flow {
            source: net.live.gateway().into(),
            destination: d.address.into(),
            protocol: frame::UDP,
            source_port: 53,
            destination_port: 5353,
        };
        let (out, _) = net.arrive(back, b"answer");
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn a_broadcast_reaches_every_other_machine_once() {
        let mut net = branch();
        let c = net.nodes[2].clone();
        let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
        arp.extend_from_slice(&c.mac);
        arp.extend_from_slice(&c.address.octets());
        arp.extend_from_slice(&[0; 6]);
        arp.extend_from_slice(&[10, 10, 10, 10]);
        let (out, _) = net.handle(
            2,
            &frame::ethernet(BROADCAST, c.mac, frame::ETHER_ARP, &arp),
        );
        let mut reached: Vec<usize> = out.iter().map(|o| o.0).collect();
        reached.sort_unstable();
        assert_eq!(reached, [0, 1, 3]);
    }

    #[test]
    fn dhcp_answers_through_a_switch() {
        let mut net = branch();
        let b = net.nodes[1].clone();
        let mut discover = vec![0u8; 240];
        discover[0..3].copy_from_slice(&[1, 1, 6]);
        discover[28..34].copy_from_slice(&b.mac);
        discover[236..240].copy_from_slice(&[99, 130, 83, 99]);
        discover.extend_from_slice(&[53, 1, 1, 255]);
        let ip = frame::ipv4_packet(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            frame::UDP,
            &frame::udp(68, 67, &discover),
        );
        let (out, _) = net.handle(
            1,
            &frame::ethernet(BROADCAST, b.mac, frame::ETHER_IPV4, &ip),
        );
        // The offer is broadcast: the switch gives it to its machines, and b finds its own.
        let offer = out.iter().find(|o| o.0 == 1).expect("b hears the offer");
        let bootp = frame::udp_data(frame::ipv4(&offer.1).unwrap().payload).to_vec();
        assert_eq!(bootp[16..20], b.address.octets());
        assert!(
            out.iter().all(|o| o.0 != 0),
            "it never crossed the router to a"
        );
    }

    /// a on the router (VLAN 1). sw1 on the router's port 100. On sw1: b and
    /// c in VLAN 10, d in VLAN 20, f on a trunk carrying 10 and 20. sw2 on
    /// sw1's port 7, by a trunk: e in VLAN 20.
    fn vlans() -> Net {
        let mut net = net();
        net.nodes.truncate(1);
        let sw1 = net.add_device(Kind::Switch, "sw1");
        let sw2 = net.add_device(Kind::Switch, "sw2");
        let end = |device, port| End { device, port };
        net.link(end(0, 100), end(sw1, 0)).unwrap();
        net.link(end(sw1, 7), end(sw2, 0)).unwrap();
        let trunk = || Mode::Trunk {
            native: 1,
            allowed: vec![10, 20],
        };
        net.topology.modes.insert(end(sw1, 7), trunk());
        net.topology.modes.insert(end(sw2, 0), trunk());
        for (name, last, at, mode) in [
            ("b", 20, end(sw1, 1), Mode::Access(10)),
            ("c", 30, end(sw1, 2), Mode::Access(10)),
            ("d", 40, end(sw1, 3), Mode::Access(20)),
            ("e", 50, end(sw2, 1), Mode::Access(20)),
            ("f", 60, end(sw1, 4), trunk()),
        ] {
            let address = Ipv4Addr::new(10, 10, 10, last);
            net.topology.modes.insert(at, mode);
            net.nodes.push(Node {
                name: name.into(),
                address,
                mac: frame::node_mac(address),
                port: PathBuf::from(name),
                at,
            });
        }
        net
    }

    /// `vlans()` with the router on a trunk carrying 10 and 20, and a
    /// network in each: b and c in 10.0.10.0/24, d and e in 10.0.20.0/24.
    fn routed() -> Net {
        let mut net = vlans();
        let trunk = Mode::Trunk {
            native: 1,
            allowed: vec![10, 20],
        };
        for end in [
            End {
                device: 0,
                port: 100,
            },
            End { device: 1, port: 0 },
        ] {
            net.topology.modes.insert(end, trunk.clone());
        }
        net.set_interface(10, Some((Ipv4Addr::new(10, 0, 10, 0), 24)))
            .unwrap();
        net.set_interface(20, Some((Ipv4Addr::new(10, 0, 20, 0), 24)))
            .unwrap();
        net.readdress();
        net
    }

    /// An echo request from node `from` to `address`, through the gateway.
    fn ping_via_gateway(net: &Net, from: usize, address: Ipv4Addr, ttl: u8) -> Vec<u8> {
        let me = &net.nodes[from];
        let mut ip =
            frame::ipv4_packet(me.address, address, frame::ICMP, &[8, 0, 0, 0, 0, 9, 0, 1]);
        ip[8] = ttl;
        frame::ethernet(GATEWAY_MAC, me.mac, frame::ETHER_IPV4, &ip)
    }

    #[test]
    fn a_conversation_keeps_its_way_and_where_it_stopped() {
        let mut net = routed();
        let (b, e) = (1, 4);
        let to_e = net.nodes[e].address;
        net.handle(b, &ping_via_gateway(&net, b, to_e, 64));
        let flows = net.flows();
        let path = &flows.iter().find(|f| f["source_node"] == "b").unwrap()["path"];
        let devices: Vec<_> = path["hops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["device"].as_str().unwrap().to_owned())
            .collect();
        assert!(
            devices.iter().any(|d| *d == net.topology.devices[0].name),
            "through the router: {devices:?}"
        );
        assert!(devices.len() >= 3, "switch, router, switch: {devices:?}");
        let router = path["hops"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["kind"] == "router")
            .unwrap();
        assert_eq!(router["vlan"], 10);
        assert_eq!(router["ttl"], 64);
        assert_eq!(path["reached"], "e");
        assert_eq!(path["verdict"], "allow");
        // Refused from now on: the same conversation's way says where it stopped.
        net.add_rule(Rule {
            verdict: Verdict::Reject,
            source: "b".into(),
            ..Rule::default()
        });
        net.handle(b, &ping_via_gateway(&net, b, to_e, 64));
        let flows = net.flows();
        let path = &flows.iter().find(|f| f["source_node"] == "b").unwrap()["path"];
        assert_eq!(path["reached"], serde_json::Value::Null);
        assert_eq!(path["verdict"], "reject");
    }

    #[test]
    fn a_capture_on_a_cable_writes_what_crossed_it() {
        let mut net = routed();
        let (b, e) = (1, 4);
        let file = std::env::temp_dir().join(format!("bb-tap-{}.pcap", std::process::id()));
        let key = cable_key(
            End {
                device: 0,
                port: 100,
            },
            End { device: 1, port: 0 },
        );
        net.captures.push(
            crate::capture::Capture::start(
                "c1".into(),
                crate::capture::Tap::Link(key),
                crate::capture::Keep::All,
                None,
                file.clone(),
            )
            .unwrap(),
        );
        let to_e = net.nodes[e].address;
        net.handle(b, &ping_via_gateway(&net, b, to_e, 64));
        net.captures[0].stop();
        let written = std::fs::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert!(net.captures[0].frames >= 1);
        // On the trunk, the frame carries its VLAN's tag.
        assert_eq!(&written[24 + 16 + 12..24 + 16 + 14], &[0x81, 0x00]);
    }

    #[test]
    fn a_network_s_ipv6_prefix_is_its_ipv4_range_said_aloud() {
        let range = (Ipv4Addr::new(10, 70, 1, 0), 24);
        assert_eq!(
            prefix6(range),
            "fd00:10:70:1::".parse::<Ipv6Addr>().unwrap()
        );
        assert_eq!(
            gateway6(range),
            "fd00:10:70:1::1".parse::<Ipv6Addr>().unwrap()
        );
        assert_eq!(
            v4_of6(prefix6((Ipv4Addr::new(192, 168, 255, 0), 24))),
            Some(Ipv4Addr::new(192, 168, 255, 1))
        );
        assert_eq!(v4_of6("2001:db8::1".parse().unwrap()), None);
    }

    #[test]
    fn ipv6_is_advertised_routed_and_judged() {
        let mut net = routed();
        let (b, e) = (1, 4);
        net.set_ipv6(0, 10, true).unwrap();
        net.set_ipv6(0, 20, true).unwrap();
        let mac = net.nodes[b].mac;
        let (ten, twenty): (Ipv6Addr, Ipv6Addr) = (
            "fd00:10:0:10::".parse().unwrap(),
            "fd00:10:0:20::".parse().unwrap(),
        );
        let v6 = |to: Mac, ip: &[u8]| frame::ethernet(to, mac, frame::ETHER_IPV6, ip);
        let icmp_of = |out: &[Out], node: usize, kind: u8| {
            out.iter().filter(|o| o.0 == node).find_map(|o| {
                frame::ipv6(&o.1)
                    .filter(|p| p.payload.first() == Some(&kind))
                    .map(|p| (o.1.clone(), p.flow))
            })
        };
        // b asks for a router, and is told the /64 to make its address in.
        let routers: Ipv6Addr = "ff02::2".parse().unwrap();
        let ask = frame::icmpv6_packet(
            frame::link_local(mac),
            routers,
            255,
            vec![frame::ROUTER_SOLICIT, 0, 0, 0, 0, 0, 0, 0],
        );
        let (out, _) = net.handle(b, &v6(frame::multicast_mac(routers), &ask));
        let (bytes, flow) = icmp_of(&out, b, frame::ROUTER_ADVERT).expect("an advertisement");
        let told = frame::ipv6(&bytes).unwrap();
        assert_eq!(frame::advertised(told.payload), Some(ten));
        let (IpAddr::V6(from), IpAddr::V6(to)) = (flow.source, flow.destination) else {
            panic!()
        };
        assert_eq!(frame::checksum6(from, to, frame::ICMPV6, told.payload), 0);
        // Who has the router's address? The router, at its MAC.
        let gateway: Ipv6Addr = "fd00:10:0:10::1".parse().unwrap();
        let mut asks = vec![frame::NEIGHBOR_SOLICIT, 0, 0, 0, 0, 0, 0, 0];
        asks.extend_from_slice(&gateway.octets());
        let ns = frame::icmpv6_packet(
            frame::link_local(mac),
            "ff02::1:ff00:1".parse().unwrap(),
            255,
            asks,
        );
        let (out, _) = net.handle(b, &v6([0x33, 0x33, 0xff, 0, 0, 1], &ns));
        let (bytes, _) = icmp_of(&out, b, frame::NEIGHBOR_ADVERT).expect("an advertisement");
        assert_eq!(
            &frame::ipv6(&bytes).unwrap().payload[8..24],
            &gateway.octets()
        );
        // b pings e by the address e makes, through the router, one hop older.
        let (me, to_e) = (
            frame::eui64(ten, mac),
            frame::eui64(twenty, net.nodes[e].mac),
        );
        let ping = |to: Ipv6Addr, hops| {
            v6(
                GATEWAY_MAC,
                &frame::icmpv6_packet(me, to, hops, frame::echo6_request(9, 1)),
            )
        };
        let (out, said) = net.handle(b, &ping(to_e, 64));
        let (got, _) = icmp_of(&out, e, frame::ECHO6_REQUEST).expect("e gets it");
        assert_eq!(got[14 + 7], 63, "a hop less");
        assert!(said.iter().any(|(name, _)| *name == "connection.created"));
        let flows = net.flows();
        let told = flows
            .iter()
            .find(|f| f["source_node"] == "b")
            .expect("told");
        assert_eq!(
            (told["destination_node"].as_str(), told["protocol"].as_str()),
            (Some("e"), Some("icmpv6"))
        );
        assert_eq!(told["source"], me.to_string());
        // The router answers its own address; a spent hop is "time exceeded".
        let (out, _) = net.handle(b, &ping(gateway, 64));
        assert!(icmp_of(&out, b, frame::ECHO6_REPLY).is_some());
        let (out, _) = net.handle(b, &ping(to_e, 1));
        assert!(icmp_of(&out, b, frame::ICMPV6_TIME_EXCEEDED).is_some());
        // A rule for IPv4 only leaves IPv6 alone; one for IPv6 stops it.
        for (family, through) in [(4, true), (6, false)] {
            let id = net.add_rule(Rule {
                verdict: Verdict::Deny,
                source: "b".into(),
                destination: "vlan:20".into(),
                protocol: Some(frame::ICMP),
                family: Some(family),
                ..Rule::default()
            });
            let (out, _) = net.handle(b, &ping(to_e, 64));
            assert_eq!(out.iter().any(|o| o.0 == e), through, "IPv{family} only");
            net.remove_rule(&id);
        }
        // A network that does not speak IPv6 is not reached by it.
        net.set_ipv6(0, 20, false).unwrap();
        let (out, _) = net.handle(b, &ping(to_e, 64));
        assert!(out.iter().all(|o| o.0 != e));
        assert!(icmp_of(&out, b, frame::ICMPV6_UNREACHABLE).is_some());
    }

    #[test]
    fn the_router_carries_between_its_networks_one_hop_at_a_time() {
        let mut net = routed();
        let (b, e) = (1, 4);
        assert_eq!(net.nodes[b].address, Ipv4Addr::new(10, 0, 10, 10));
        assert_eq!(net.nodes[e].address, Ipv4Addr::new(10, 0, 20, 11));
        // b in VLAN 10 to e in VLAN 20, two switches away: routed, rewritten,
        // one hop older.
        let to_e = net.nodes[e].address;
        let (out, said) = net.handle(b, &ping_via_gateway(&net, b, to_e, 64));
        let (to, bytes) = out.iter().find(|o| o.0 == e).expect("e gets it");
        assert_eq!(*to, e);
        assert_eq!(frame::destination(bytes), net.nodes[e].mac);
        assert_eq!(frame::source(bytes), GATEWAY_MAC);
        assert_eq!(bytes[14 + 8], 63, "TTL one less");
        assert_eq!(frame::checksum(&bytes[14..34]), 0, "header checksum right");
        assert!(said.iter().any(|(name, _)| *name == "connection.created"));
        // The last hop spent: "time exceeded" back to b, from its own gateway.
        let (out, _) = net.handle(b, &ping_via_gateway(&net, b, to_e, 1));
        assert!(out.iter().all(|o| o.0 != e));
        let back = out.iter().find(|o| o.0 == b).expect("an answer for b");
        let packet = frame::ipv4(&back.1).unwrap();
        assert_eq!(packet.flow.source, Ipv4Addr::new(10, 0, 10, 1));
        assert_eq!(packet.payload[0], frame::ICMP_TIME_EXCEEDED);
        // Nobody at that address: unreachable.
        let (out, _) = net.handle(
            b,
            &ping_via_gateway(&net, b, Ipv4Addr::new(10, 0, 20, 99), 64),
        );
        let back = out.iter().find(|o| o.0 == b).expect("an answer for b");
        assert_eq!(
            frame::ipv4(&back.1).unwrap().payload[0],
            frame::ICMP_UNREACHABLE
        );
        // A rule between the two networks stops it.
        net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Deny,
            source: "10.0.10.0/24".into(),
            destination: "e".into(),
            protocol: None,
            ports: None,
            ..Rule::default()
        });
        let (out, _) = net.handle(b, &ping_via_gateway(&net, b, to_e, 64));
        assert!(out.iter().all(|o| o.0 != e), "denied");
        // The router answers on any of its addresses, and a VLAN with no
        // network of its own (VLAN 1 has the main one) still has its gateway.
        let (out, _) = net.handle(
            b,
            &ping_via_gateway(&net, b, Ipv4Addr::new(10, 0, 20, 1), 64),
        );
        assert!(
            out.iter()
                .any(|o| o.0 == b && frame::ipv4(&o.1).unwrap().payload[0] == 0)
        );
    }

    #[test]
    fn each_cable_a_frame_crosses_adds_what_it_does() {
        let mut net = routed();
        let (b, e) = (1, 4);
        let ms = |latency_ms| Traffic {
            latency_ms,
            ..Traffic::default()
        };
        let end = |device, port| End { device, port };
        net.link_traffic
            .insert(cable_key(end(0, 100), end(1, 0)), ms(10.0));
        net.link_traffic
            .insert(cable_key(end(1, 7), end(2, 0)), ms(5.0));
        net.node_traffic.insert("b".into(), ms(1.0));
        net.node_traffic.insert("e".into(), ms(2.0));
        net.live.traffic = ms(100.0);
        let to_e = net.nodes[e].address;
        let (out, _) = net.handle(b, &ping_via_gateway(&net, b, to_e, 64));
        let at = out.iter().position(|o| o.0 == e).expect("e gets it");
        // b's cable, up to the router and back down the same trunk, across to
        // sw2, e's cable, and the network's own: 1 + 10 + 10 + 5 + 2 + 100.
        assert_eq!(net.take_costs()[at].latency_ms, 128.0);
        assert_eq!(net.link_frames[&cable_key(end(0, 100), end(1, 0))], 2);
        assert_eq!(net.node_frames["b"].0, 1);
        assert_eq!(net.node_frames["e"].1, 1);
    }

    #[test]
    fn traffic_one_after_another_adds_compounds_and_narrows() {
        let a = Traffic {
            latency_ms: 10.0,
            jitter_ms: 1.0,
            loss_percent: 10.0,
            bandwidth_kbps: 1000.0,
        };
        let b = Traffic {
            latency_ms: 5.0,
            jitter_ms: 0.0,
            loss_percent: 10.0,
            bandwidth_kbps: 0.0,
        };
        let both = a.then(b);
        assert_eq!(both.latency_ms, 15.0);
        assert!((both.loss_percent - 19.0).abs() < 1e-9);
        assert_eq!(both.bandwidth_kbps, 1000.0, "0 is unlimited");
        assert!(Traffic::default().is_perfect());
    }

    /// Two routers: `gw` with `a` on it, and `r2` with `c`, joined by a
    /// cable in VLAN 99 where they share 10.99.0.0/24 (gw .1, r2 .2); r2's
    /// own network is 10.20.0.0/24, and each has a route to the other side.
    fn two_routers() -> Net {
        let mut net = net();
        net.nodes.truncate(1);
        let r2 = net.add_device(Kind::Router, "r2");
        let end = |device, port| End { device, port };
        net.link(end(0, 100), end(r2, 0)).unwrap();
        net.topology.modes.insert(end(0, 100), Mode::Access(99));
        net.topology.modes.insert(end(r2, 0), Mode::Access(99));
        let side = |a, b, c, d, prefix, last| Side {
            range: (Ipv4Addr::new(a, b, c, d), prefix),
            address: Ipv4Addr::new(a, b, c, last),
        };
        net.set_network(0, 99, Some(side(10, 99, 0, 0, 24, 1)))
            .unwrap();
        net.set_network(r2, 99, Some(side(10, 99, 0, 0, 24, 2)))
            .unwrap();
        net.set_network(r2, 1, Some(side(10, 20, 0, 0, 24, 1)))
            .unwrap();
        let address = Ipv4Addr::new(10, 20, 0, 10);
        net.nodes.push(Node {
            name: "c".into(),
            address,
            mac: frame::node_mac(address),
            port: PathBuf::from("c"),
            at: end(r2, 5),
        });
        let route = |to: [u8; 4], prefix, via: [u8; 4]| Route {
            to: (Ipv4Addr::from(to), prefix),
            via: Ipv4Addr::from(via),
        };
        net.add_route(0, route([10, 20, 0, 0], 24, [10, 99, 0, 2]))
            .unwrap();
        net.add_route(r2, route([0, 0, 0, 0], 0, [10, 99, 0, 1]))
            .unwrap();
        net
    }

    #[test]
    fn ipv6_goes_between_routers_the_way_ipv4_does() {
        let mut net = two_routers();
        let (a, c, r2) = (0, 1, 1);
        for (router, vlan) in [(0, 1), (0, 99), (r2, 99), (r2, 1)] {
            net.set_ipv6(router, vlan, true).unwrap();
        }
        let here = prefix6((net.live.network, net.live.prefix));
        let there = prefix6((Ipv4Addr::new(10, 20, 0, 0), 24));
        let (to_a, to_c) = (
            frame::eui64(here, net.nodes[a].mac),
            frame::eui64(there, net.nodes[c].mac),
        );
        let ping = |net: &Net, from: usize, router: Mac, to: Ipv6Addr, kind: u8| {
            let me = frame::eui64(if from == a { here } else { there }, net.nodes[from].mac);
            let mut echo = frame::echo6_request(9, 1);
            echo[0] = kind;
            let ip = frame::icmpv6_packet(me, to, 64, echo);
            frame::ethernet(router, net.nodes[from].mac, frame::ETHER_IPV6, &ip)
        };
        let (out, _) = net.handle(
            c,
            &ping(&net, c, net.router_mac(r2), to_a, frame::ECHO6_REQUEST),
        );
        let got = &out.iter().find(|o| o.0 == a).expect("a gets it").1;
        assert_eq!(got[14 + 7], 62, "two routers, two hops");
        let (out, _) = net.handle(a, &ping(&net, a, GATEWAY_MAC, to_c, frame::ECHO6_REPLY));
        assert!(out.iter().any(|o| o.0 == c), "and the answer comes back");
        // The way between them not speaking IPv6: no way.
        net.set_ipv6(r2, 99, false).unwrap();
        let (out, _) = net.handle(
            c,
            &ping(&net, c, net.router_mac(r2), to_a, frame::ECHO6_REQUEST),
        );
        assert!(out.iter().all(|o| o.0 != a));
    }

    /// A TCP SYN from node `from` to `to`:`port`, handed to the router `mac`.
    fn syn_via(net: &Net, from: usize, mac: Mac, to: Ipv4Addr, port: u16) -> Vec<u8> {
        let me = &net.nodes[from];
        let [high, low] = port.to_be_bytes();
        let mut tcp = vec![
            0x9c, 0x40, high, low, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0, 0,
        ];
        let mut pseudo = [me.address.octets(), to.octets()].concat();
        pseudo.extend_from_slice(&[0, frame::TCP, 0, 20]);
        pseudo.extend_from_slice(&tcp);
        tcp[16..18].copy_from_slice(&frame::checksum(&pseudo).to_be_bytes());
        let ip = frame::ipv4_packet(me.address, to, frame::TCP, &tcp);
        frame::ethernet(mac, me.mac, frame::ETHER_IPV4, &ip)
    }

    #[test]
    fn a_hidden_network_leaves_as_its_router_and_takes_only_answers() {
        let mut net = two_routers();
        let (a, c) = (0, 1);
        let r2 = 1;
        let r2_mac = net.router_mac(r2);
        let outside = Ipv4Addr::new(10, 99, 0, 2);
        // The way between the routers is not r2's to hide.
        assert!(net.set_hidden(r2, 99, true).is_err());
        net.set_hidden(r2, 1, true).unwrap();
        // c pings a: a sees r2, not c.
        let to_a = net.nodes[a].address;
        let mut ping = ping_via_gateway(&net, c, to_a, 64);
        ping[0..6].copy_from_slice(&r2_mac);
        let (out, _) = net.handle(c, &ping);
        let got = &out.iter().find(|o| o.0 == a).expect("a gets it").1;
        let seen = frame::ipv4(got).unwrap().flow;
        assert_eq!(seen.source, outside);
        assert_eq!(frame::checksum(&got[14..34]), 0);
        let flows = net.flows();
        let path = &flows
            .iter()
            .find(|f| f["source_node"] == "c")
            .expect("told as c's")["path"];
        assert_eq!(path["nat"]["kind"], "hide");
        assert_eq!(path["nat"]["router"], "r2");
        assert!(
            flows.iter().all(|f| f["source"] != outside.to_string()),
            "told once"
        );
        // a answers r2, and the answer reaches c.
        let mut reply = got.clone();
        reply[0..6].copy_from_slice(&GATEWAY_MAC);
        reply[6..12].copy_from_slice(&net.nodes[a].mac);
        let mut ip = reply[14..].to_vec();
        ip[12..16].copy_from_slice(&to_a.octets());
        ip[16..20].copy_from_slice(&outside.octets());
        ip[20] = 0;
        frame::rewrite(&mut ip, None, None);
        reply.truncate(14);
        reply.extend_from_slice(&ip);
        let (out, _) = net.handle(a, &reply);
        let back = &out.iter().find(|o| o.0 == c).expect("c gets the answer").1;
        assert_eq!(
            frame::ipv4(back).unwrap().flow.destination,
            net.nodes[c].address
        );
        let asked = |net: &Net| {
            net.flows()
                .into_iter()
                .find(|f| f["source_node"] == "c" && f["source_port"] == 9)
        };
        assert_eq!(asked(&net).expect("c's ping")["replied"], true);
        // A probe that dies at gw: gw tells r2, and r2 tells c, about c's
        // own packet -- traceroute sees past the router.
        let mut probe = ping_via_gateway(&net, c, to_a, 2);
        probe[0..6].copy_from_slice(&r2_mac);
        let (out, _) = net.handle(c, &probe);
        let told = &out.iter().find(|o| o.0 == c).expect("c hears it died").1;
        let packet = frame::ipv4(told).unwrap();
        assert_eq!(packet.payload[0], frame::ICMP_TIME_EXCEEDED);
        assert_eq!(packet.flow.destination, net.nodes[c].address);
        assert_eq!(frame::checksum(&told[14..34]), 0);
        assert_eq!(frame::checksum(packet.payload), 0, "the ICMP checksum");
        let quoted = &packet.payload[8..];
        assert_eq!(
            &quoted[12..16],
            &net.nodes[c].address.octets(),
            "about c's own packet"
        );
        assert_eq!(frame::checksum(&quoted[..20]), 0, "the quoted header");
        // a cannot begin anything with c itself.
        let to_c = net.nodes[c].address;
        let (out, _) = net.handle(a, &ping_via_gateway(&net, a, to_c, 64));
        assert!(out.iter().all(|o| o.0 != c));
        // Showing the network again keeps what was told.
        net.set_hidden(r2, 1, false).unwrap();
        assert!(asked(&net).is_some());
    }

    #[test]
    fn a_forward_lets_one_port_in_and_answers_from_the_router() {
        let mut net = two_routers();
        let (a, c) = (0, 1);
        let r2 = 1;
        let outside = Ipv4Addr::new(10, 99, 0, 2);
        net.set_hidden(r2, 1, true).unwrap();
        net.add_forward(
            r2,
            Forward {
                id: String::new(),
                protocol: frame::TCP,
                port: 8080,
                to: "c".into(),
                to_port: 80,
                uses: 0,
                last: None,
            },
        )
        .unwrap();
        let (out, _) = net.handle(a, &syn_via(&net, a, GATEWAY_MAC, outside, 8080));
        // The same SYN again is the same conversation: one use, not two.
        net.handle(a, &syn_via(&net, a, GATEWAY_MAC, outside, 8080));
        assert_eq!(net.routers["r2"].forwards[0].uses, 1);
        let got = &out.iter().find(|o| o.0 == c).expect("c gets it").1;
        let seen = frame::ipv4(got).unwrap().flow;
        assert_eq!(
            (seen.destination4(), seen.destination_port),
            (net.nodes[c].address, 80)
        );
        assert_eq!(seen.source, net.nodes[a].address);
        let flows = net.flows();
        let path = &flows
            .iter()
            .find(|f| f["destination_node"] == "c")
            .expect("told as a to c")["path"];
        assert_eq!(path["nat"]["kind"], "forward");
        assert_eq!(path["reached"], "c");
        // c's answer comes back from r2's door.
        let to_a = net.nodes[a].address;
        let mut synack = syn_via(&net, c, net.router_mac(r2), to_a, 0x9c40);
        let mut ip = synack[14..].to_vec();
        ip[20..22].copy_from_slice(&80u16.to_be_bytes());
        frame::rewrite(&mut ip, None, None);
        synack.truncate(14);
        synack.extend_from_slice(&ip);
        let (out, _) = net.handle(c, &synack);
        let back = &out.iter().find(|o| o.0 == a).expect("a gets the answer").1;
        let flow = frame::ipv4(back).unwrap().flow;
        assert_eq!((flow.source4(), flow.source_port), (outside, 8080));
        // Another port stays shut.
        let (out, _) = net.handle(a, &syn_via(&net, a, GATEWAY_MAC, outside, 22));
        assert!(out.iter().all(|o| o.0 != c));
    }

    #[test]
    fn two_routers_carry_between_their_networks_hop_by_hop() {
        let mut net = two_routers();
        let (a, c) = (0, 1);
        let r2_mac = net.router_mac(1);
        assert_ne!(r2_mac, GATEWAY_MAC);
        assert_eq!(
            net.serving(c).map(|(router, vlan, _)| (router, vlan)),
            Some((1, 1))
        );
        assert_eq!(net.serving(a).map(|(router, _, _)| router), Some(0));
        // a to c: gw, then r2, then c -- two hops older, from r2.
        let to_c = net.nodes[c].address;
        let (out, _) = net.handle(a, &ping_via_gateway(&net, a, to_c, 64));
        let got = &out.iter().find(|o| o.0 == c).expect("c gets it").1;
        assert_eq!(frame::source(got), r2_mac);
        assert_eq!(got[14 + 8], 62);
        // And back: c to its gateway r2, the default route to gw, then a.
        let to_a = net.nodes[a].address;
        let mut back = ping_via_gateway(&net, c, to_a, 64);
        back[0..6].copy_from_slice(&r2_mac);
        let (out, _) = net.handle(c, &back);
        let got = &out.iter().find(|o| o.0 == a).expect("a gets it").1;
        assert_eq!(got[14 + 8], 62);
        // traceroute's second probe, under Deny with only a to c allowed:
        // r2 says "time exceeded", and gw lets that through to a.
        net.set_default(false);
        net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Allow,
            source: "a".into(),
            destination: "c".into(),
            protocol: None,
            ports: None,
            ..Rule::default()
        });
        let (out, _) = net.handle(a, &ping_via_gateway(&net, a, to_c, 2));
        let got = &out.iter().find(|o| o.0 == a).expect("an answer for a").1;
        let packet = frame::ipv4(got).unwrap();
        assert_eq!(packet.flow.source, Ipv4Addr::new(10, 99, 0, 2));
        assert_eq!(packet.payload[0], frame::ICMP_TIME_EXCEEDED);
        // c asks for its gateway, r2, and r2 answers with its own MAC.
        let me = &net.nodes[c];
        let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
        arp.extend_from_slice(&me.mac);
        arp.extend_from_slice(&me.address.octets());
        arp.extend_from_slice(&[0; 6]);
        arp.extend_from_slice(&[10, 20, 0, 1]);
        let (out, _) = net.handle(
            c,
            &frame::ethernet(BROADCAST, me.mac, frame::ETHER_ARP, &arp),
        );
        let reply = out.iter().find(|o| o.0 == c).expect("an ARP reply");
        assert_eq!(frame::arp(&reply.1).unwrap().sender_mac, r2_mac);
    }

    #[test]
    fn a_route_needs_a_next_hop_on_one_of_the_routers_networks() {
        let mut net = two_routers();
        let route = |via: [u8; 4]| Route {
            to: (Ipv4Addr::new(172, 16, 0, 0), 12),
            via: Ipv4Addr::from(via),
        };
        assert_eq!(
            net.add_route(1, route([10, 10, 10, 5])).unwrap_err().code,
            "network.route_via"
        );
        assert_eq!(
            net.add_route(1, route([10, 99, 0, 2])).unwrap_err().code,
            "network.route_via",
            "not itself"
        );
        // Sharing a network is for two routers at different addresses.
        let shared = Side {
            range: (Ipv4Addr::new(10, 99, 0, 0), 24),
            address: Ipv4Addr::new(10, 99, 0, 3),
        };
        assert_eq!(
            net.set_network(1, 98, Some(shared)).unwrap_err().code,
            "network.overlap",
            "the same router twice"
        );
        assert!(net.remove_route(1, (Ipv4Addr::UNSPECIFIED, 0)));
    }

    #[test]
    fn networks_on_the_router_never_share_an_address() {
        let mut net = net();
        let range = |a, p| Some((Ipv4Addr::new(10, a, 0, 0), p));
        assert_eq!(
            net.set_interface(10, Some((Ipv4Addr::new(10, 10, 10, 128), 25)))
                .unwrap_err()
                .code,
            "network.overlap"
        );
        net.set_interface(10, range(20, 24)).unwrap();
        assert_eq!(
            net.set_interface(20, range(20, 16)).unwrap_err().code,
            "network.overlap"
        );
        assert_eq!(
            net.set_interface(1, range(30, 24)).unwrap_err().code,
            "network.main_vlan"
        );
        // Changing VLAN 10's own range is not an overlap with itself.
        net.set_interface(10, range(20, 23)).unwrap();
        assert!(net.outside(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!net.outside(Ipv4Addr::new(10, 20, 1, 5)));
    }

    fn arp_from(net: &Net, from: usize) -> Vec<u8> {
        let me = &net.nodes[from];
        let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
        arp.extend_from_slice(&me.mac);
        arp.extend_from_slice(&me.address.octets());
        arp.extend_from_slice(&[0; 6]);
        arp.extend_from_slice(&[10, 10, 10, 99]);
        frame::ethernet(BROADCAST, me.mac, frame::ETHER_ARP, &arp)
    }

    fn reached(out: &[Out]) -> Vec<usize> {
        let mut who: Vec<usize> = out.iter().map(|o| o.0).collect();
        who.sort_unstable();
        who
    }

    #[test]
    fn a_vlan_keeps_its_machines_to_themselves() {
        let mut net = vlans();
        // b to c, both VLAN 10: carried. b to d, VLAN 20: not.
        assert_eq!(reached(&net.handle(1, &ping(&net, 1, 2, 7)).0), [2]);
        assert!(net.handle(1, &ping(&net, 1, 3, 7)).0.is_empty());
        // A broadcast from b reaches c, and f (whose trunk carries 10) tagged,
        // and nobody in VLAN 20 or on the router's VLAN 1.
        let (out, _) = net.handle(1, &arp_from(&net, 1));
        assert_eq!(reached(&out), [2, 5]);
        let to_f = out.iter().find(|o| o.0 == 5).unwrap();
        assert_eq!(
            frame::untag(&to_f.1).map(|(vlan, _)| vlan),
            Some(10),
            "tagged on f's trunk"
        );
        let to_c = out.iter().find(|o| o.0 == 2).unwrap();
        assert_eq!(
            frame::ethertype(&to_c.1),
            Some(frame::ETHER_ARP),
            "untagged on c's access port"
        );
    }

    #[test]
    fn a_trunk_carries_vlans_between_switches() {
        let mut net = vlans();
        // d (VLAN 20, sw1) to e (VLAN 20, sw2): across the trunk.
        assert_eq!(reached(&net.handle(3, &ping(&net, 3, 4, 7)).0), [4]);
        // b (VLAN 10) to e: not, though the trunk carries 10 too.
        assert!(net.handle(1, &ping(&net, 1, 4, 7)).0.is_empty());
    }

    #[test]
    fn a_machine_on_a_trunk_speaks_with_tags() {
        let mut net = vlans();
        // f tags its ping to b with VLAN 10; b gets it untagged.
        let tagged = frame::tag(&ping(&net, 5, 1, 7), 10);
        let (out, _) = net.handle(5, &tagged);
        assert_eq!(reached(&out), [1]);
        assert_eq!(frame::ethertype(&out[0].1), Some(frame::ETHER_IPV4));
        // b's answer comes back to f tagged.
        let (out, _) = net.handle(1, &reply(&net, 1, 5, 7));
        assert_eq!(frame::untag(&out[0].1).map(|(vlan, _)| vlan), Some(10));
        // A VLAN the trunk does not carry is not taken.
        assert!(
            net.handle(5, &frame::tag(&ping(&net, 5, 1, 8), 30))
                .0
                .is_empty()
        );
        // And a tag hides nothing from port security: another address inside.
        let mut forged = ping(&net, 5, 1, 9);
        forged[29] = 99;
        assert!(net.handle(5, &frame::tag(&forged, 10)).0.is_empty());
        // A tag on an access port is not taken either.
        assert!(
            net.handle(1, &frame::tag(&ping(&net, 1, 2, 9), 10))
                .0
                .is_empty()
        );
    }

    #[test]
    fn two_ends_that_disagree_leak_into_the_other_vlan_as_real_switches_do() {
        let mut net = vlans();
        let (sw1, sw2) = (1, 2);
        // The cable between the switches becomes access 10 on one end and
        // access 20 on the other: what leaves untagged in 10 arrives in 20.
        net.topology.modes.insert(
            End {
                device: sw1,
                port: 7,
            },
            Mode::Access(10),
        );
        net.topology.modes.insert(
            End {
                device: sw2,
                port: 0,
            },
            Mode::Access(20),
        );
        let (out, _) = net.handle(1, &arp_from(&net, 1));
        assert!(
            reached(&out).contains(&4),
            "b's broadcast reached e in VLAN 20"
        );
    }

    #[test]
    fn a_vlan_with_no_router_gets_no_address() {
        let mut net = vlans();
        let b = net.nodes[1].clone();
        let mut discover = vec![0u8; 240];
        discover[0..3].copy_from_slice(&[1, 1, 6]);
        discover[28..34].copy_from_slice(&b.mac);
        discover[236..240].copy_from_slice(&[99, 130, 83, 99]);
        discover.extend_from_slice(&[53, 1, 1, 255]);
        let ip = frame::ipv4_packet(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            frame::UDP,
            &frame::udp(68, 67, &discover),
        );
        let (out, _) = net.handle(
            1,
            &frame::ethernet(BROADCAST, b.mac, frame::ETHER_IPV4, &ip),
        );
        // The router hangs on sw1's port 0 in VLAN 1: b's VLAN 10 never gets there.
        assert!(
            out.iter()
                .all(|o| frame::ipv4(&o.1).is_none_or(|p| p.flow.source_port != 67)),
            "no offer"
        );
        // Put the router's side of the cable in VLAN 10, and it answers.
        net.topology
            .modes
            .insert(End { device: 1, port: 0 }, Mode::Access(10));
        net.topology.modes.insert(
            End {
                device: 0,
                port: 100,
            },
            Mode::Access(10),
        );
        let ask = frame::ethernet(BROADCAST, b.mac, frame::ETHER_IPV4, &ip);
        // There now, but the router has no network in VLAN 10.
        let (out, _) = net.handle(1, &ask);
        assert!(out.iter().all(|o| o.0 != 1), "no network, no offer");
        // Give VLAN 10 one: b moves into it, and is offered its new address.
        net.set_interface(10, Some((Ipv4Addr::new(10, 0, 10, 0), 24)))
            .unwrap();
        let moved = net.readdress();
        // b and c, the two machines in VLAN 10, in their order.
        assert_eq!(moved.len(), 2);
        assert_eq!(
            (moved[0].0.as_str(), moved[0].2),
            ("b", Ipv4Addr::new(10, 0, 10, 10))
        );
        let (out, _) = net.handle(1, &ask);
        let offer = out.iter().find(|o| o.0 == 1).expect("an offer for b");
        let dhcp = &offer.1[14 + 20 + 8..];
        assert_eq!(&dhcp[16..20], &[10, 0, 10, 10]);
        assert_eq!(&dhcp[20..24], &[10, 0, 10, 1], "from VLAN 10's gateway");
    }

    #[test]
    fn a_renewal_is_answered_unicast_from_the_address_it_asked() {
        let mut net = routed();
        let b = net.nodes[1].clone();
        let renew = |held: Ipv4Addr, server: Ipv4Addr| {
            let mut request = vec![0u8; 240];
            request[0..3].copy_from_slice(&[1, 1, 6]);
            request[12..16].copy_from_slice(&held.octets());
            request[28..34].copy_from_slice(&b.mac);
            request[236..240].copy_from_slice(&[99, 130, 83, 99]);
            request.extend_from_slice(&[53, 1, 3, 255]);
            let ip = frame::ipv4_packet(held, server, frame::UDP, &frame::udp(68, 67, &request));
            frame::ethernet(GATEWAY_MAC, b.mac, frame::ETHER_IPV4, &ip)
        };
        let gateway = Ipv4Addr::new(10, 0, 10, 1);
        // Renewing the address it has: an ACK, to it, from its gateway.
        let (out, _) = net.handle(1, &renew(b.address, gateway));
        let ack = &out.iter().find(|o| o.0 == 1).expect("an answer").1;
        let packet = frame::ipv4(ack).unwrap();
        assert_eq!(frame::destination(ack), b.mac);
        assert_eq!(
            (packet.flow.source4(), packet.flow.destination4()),
            (gateway, b.address)
        );
        assert_eq!(frame::udp_data(packet.payload)[242], frame::DHCP_ACK);
        // Renewing an address it had in another network, of that network's
        // router: refused, to the old address, from the server it asked.
        let old = Ipv4Addr::new(10, 0, 20, 10);
        let old_server = Ipv4Addr::new(10, 0, 20, 1);
        let (out, _) = net.handle(1, &renew(old, old_server));
        let nak = &out.iter().find(|o| o.0 == 1).expect("an answer").1;
        let packet = frame::ipv4(nak).unwrap();
        assert_eq!(
            (packet.flow.source4(), packet.flow.destination4()),
            (old_server, old)
        );
        assert_eq!(frame::udp_data(packet.payload)[242], frame::DHCP_NAK);
    }

    #[test]
    fn a_machine_that_moved_may_ask_for_its_old_gateway_until_it_renews() {
        let mut net = routed();
        // b was 10.10.10.20 before VLAN 10 had a network (`routed`).
        let (b, old) = (1, Ipv4Addr::new(10, 10, 10, 20));
        let ask = |net: &Net, target: Ipv4Addr| {
            let me = &net.nodes[b];
            let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
            arp.extend_from_slice(&me.mac);
            arp.extend_from_slice(&old.octets());
            arp.extend_from_slice(&[0; 6]);
            arp.extend_from_slice(&target.octets());
            frame::ethernet(BROADCAST, me.mac, frame::ETHER_ARP, &arp)
        };
        let answered = |out: &[Out]| {
            out.iter()
                .any(|o| o.0 == b && frame::arp(&o.1).is_some_and(|a| a.operation == 2))
        };
        // As its old address, for the old gateway: answered.
        let (out, _) = net.handle(b, &ask(&net, Ipv4Addr::new(10, 10, 10, 1)));
        assert!(answered(&out));
        // As its old address, for anyone else: refused at the port.
        let refused = net.counters.refused;
        net.handle(b, &ask(&net, Ipv4Addr::new(10, 10, 10, 30)));
        assert_eq!(net.counters.refused, refused + 1);
        // Once it takes an address, the old one is over.
        net.moved_from.clear();
        let (out, _) = net.handle(b, &ask(&net, Ipv4Addr::new(10, 10, 10, 1)));
        assert!(!answered(&out));
    }

    #[test]
    fn a_protocol_the_firewall_cannot_name_follows_its_default() {
        let mut net = net();
        let mut other = ping(&net, 0, 1, 7);
        other[12..14].copy_from_slice(&[0x88, 0xb5]);
        // Allow: carried, as any frame between two machines.
        let (out, _) = net.handle(0, &other);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 1);
        // Deny: stopped, so no rule written for IP is walked around by it.
        net.set_default(false);
        assert!(net.handle(0, &other).0.is_empty());
        assert_eq!(net.counters.blocked, 1);
        assert_eq!(net.counters.refused, 0);
    }

    #[test]
    fn ipv6_between_two_machines_is_judged_like_ipv4() {
        let mut net = net();
        let (a, b) = (net.nodes[0].clone(), net.nodes[1].clone());
        let ip = frame::icmpv6_packet(
            frame::link_local(a.mac),
            frame::link_local(b.mac),
            64,
            frame::echo6_request(7, 1),
        );
        let six = frame::ethernet(b.mac, a.mac, frame::ETHER_IPV6, &ip);
        let (out, _) = net.handle(0, &six);
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), vec![1]);
        let flows = net.flows();
        assert_eq!(
            flows[0]["destination_node"], "b",
            "named by its link-local address"
        );
        net.set_default(false);
        assert!(net.handle(0, &six).0.is_empty());
        // Nobody sends as somebody else.
        let mut forged = six.clone();
        forged[22..38].copy_from_slice(&frame::link_local(b.mac).octets());
        assert!(net.handle(0, &forged).0.is_empty());
        assert_eq!(net.counters.refused, 1);
    }

    #[test]
    fn reset_keeps_the_cables_and_drops_the_rules() {
        let mut net = net();
        net.add_rule(Rule {
            id: String::new(),
            verdict: Verdict::Deny,
            source: "any".into(),
            destination: "any".into(),
            protocol: None,
            ports: None,
            ..Rule::default()
        });
        net.live.traffic.latency_ms = 100.0;
        net.reset();
        assert!(net.live.rules.is_empty());
        assert_eq!(net.live.traffic, Traffic::default());
        assert_eq!(net.nodes.len(), 2);
        assert_eq!(net.handle(0, &ping(&net, 0, 1, 7)).0.len(), 1);
    }

    #[test]
    fn the_shaper_loses_and_delays_as_told() {
        let mut shaper = Shaper::new();
        let lossy = Traffic {
            loss_percent: 100.0,
            ..Traffic::default()
        };
        assert!(!shaper.schedule(lossy, "x".into(), vec![0; 100]));
        let slow = Traffic {
            latency_ms: 50.0,
            ..Traffic::default()
        };
        assert!(shaper.schedule(slow, "x".into(), vec![0; 100]));
        let (ready, wait) = shaper.due();
        assert!(ready.is_empty());
        assert!(wait.unwrap() > Duration::from_millis(40));
        // 8 kbit/s: 1000 bytes is a second of cable, so the next waits.
        let thin = Traffic {
            bandwidth_kbps: 8.0,
            ..Traffic::default()
        };
        assert!(shaper.schedule(thin, "y".into(), vec![0; 1000]));
        assert!(shaper.schedule(thin, "y".into(), vec![0; 10]));
        assert!(!shaper.schedule(thin, "y".into(), vec![0; 10]));
    }

    /// The lab's own settings: jitter far wider than a frame's time on the
    /// cable. The frames still leave in the order they came.
    #[test]
    fn jitter_never_reorders_a_cable() {
        let mut shaper = Shaper::new();
        let lab = Traffic {
            bandwidth_kbps: 20_000.0,
            latency_ms: 1.0,
            jitter_ms: 8.0,
            ..Traffic::default()
        };
        for each in 0..200u8 {
            assert!(shaper.schedule(lab, "x".into(), vec![each; 100]));
        }
        std::thread::sleep(Duration::from_millis(20));
        let (ready, _) = shaper.due();
        let order: Vec<u8> = ready.iter().map(|p| p.bytes[0]).collect();
        assert_eq!(order, (0..200).collect::<Vec<u8>>());
    }
}
