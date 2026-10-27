//! Just enough Ethernet for a switch with a gateway: ARP, IPv4/IPv6, ICMP
//! echo, neighbour discovery, router adverts, UDP, DHCP and DNS.
//!
//! Its own crate because both the network's switch and the Server's internal
//! services (machine-less nodes on that switch) need it. Std only.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An Ethernet address.
pub type Mac = [u8; 6];
/// Everybody on the segment.
pub const BROADCAST: Mac = [0xff; 6];
/// The gateway's own address. Locally administered, and not one
/// [`node_mac`] can produce, because no node's address ends in 255.255.254.
pub const GATEWAY_MAC: Mac = [0x02, 0xbb, 0xff, 0xff, 0xff, 0xfe];

/// EtherType of ARP.
pub const ETHER_ARP: u16 = 0x0806;
/// EtherType of IPv4.
pub const ETHER_IPV4: u16 = 0x0800;
/// EtherType of IPv6.
pub const ETHER_IPV6: u16 = 0x86dd;
/// EtherType of an 802.1Q tag: a VLAN id follows.
pub const ETHER_VLAN: u16 = 0x8100;
/// IP protocol numbers.
pub const ICMP: u8 = 1;
/// See [`ICMP`].
pub const TCP: u8 = 6;
/// See [`ICMP`].
pub const UDP: u8 = 17;
/// ICMP for IPv6: its echo, its errors, and neighbour discovery.
pub const ICMPV6: u8 = 58;

/// MAC derived from the IP, so you can read one off the other: 10.10.10.20 -> 02:bb:0a:0a:0a:14.
pub fn node_mac(address: Ipv4Addr) -> Mac {
    let [a, b, c, d] = address.octets();
    [0x02, 0xbb, a, b, c, d]
}

/// `02:bb:0a:0a:0a:14`.
pub fn mac_text(mac: Mac) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A MAC as [`mac_text`] writes it, read back; `None` for anything else.
pub fn parse_mac(text: &str) -> Option<Mac> {
    let mut mac = [0u8; 6];
    let mut parts = text.split(':');
    for byte in &mut mac {
        let part = parts.next().filter(|part| part.len() == 2)?;
        *byte = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(mac)
}

/// Where it is going.
pub fn destination(frame: &[u8]) -> Mac {
    frame[0..6].try_into().expect("six bytes")
}

/// Where it says it came from.
pub fn source(frame: &[u8]) -> Mac {
    frame[6..12].try_into().expect("six bytes")
}

/// The frame with an 802.1Q tag for `vlan` after the two addresses.
pub fn tag(frame: &[u8], vlan: u16) -> Vec<u8> {
    let mut tagged = Vec::with_capacity(frame.len() + 4);
    tagged.extend_from_slice(&frame[..12.min(frame.len())]);
    tagged.extend_from_slice(&ETHER_VLAN.to_be_bytes());
    tagged.extend_from_slice(&(vlan & 0x0fff).to_be_bytes());
    tagged.extend_from_slice(frame.get(12..).unwrap_or_default());
    tagged
}

/// A tagged frame's VLAN id and the frame without its tag; `None` for an
/// untagged frame.
pub fn untag(frame: &[u8]) -> Option<(u16, Vec<u8>)> {
    if frame.len() < 18 || ethertype(frame) != Some(ETHER_VLAN) {
        return None;
    }
    let vlan = u16::from_be_bytes([frame[14], frame[15]]) & 0x0fff;
    let mut plain = Vec::with_capacity(frame.len() - 4);
    plain.extend_from_slice(&frame[..12]);
    plain.extend_from_slice(&frame[16..]);
    Some((vlan, plain))
}

/// What it carries, or `None` for something too short to be a frame.
pub fn ethertype(frame: &[u8]) -> Option<u16> {
    (frame.len() >= 14).then(|| u16::from_be_bytes([frame[12], frame[13]]))
}

/// Whether it is for everybody (broadcast, or IPv4/IPv6 multicast).
pub fn is_group(mac: Mac) -> bool {
    mac[0] & 1 == 1
}

/// A frame around `payload`.
pub fn ethernet(to: Mac, from: Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&to);
    frame.extend_from_slice(&from);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// The Internet checksum.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = data
        .chunks(2)
        .map(|pair| u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)])))
        .sum();
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// `ip` (a whole IPv4 packet) with a new source and/or destination -- the
/// address and the port, or a ping's identifier -- and every checksum made
/// right again: what a router doing NAT writes. A UDP checksum of zero
/// ("none") stays none.
pub fn rewrite(
    ip: &mut [u8],
    source: Option<(Ipv4Addr, u16)>,
    destination: Option<(Ipv4Addr, u16)>,
) {
    let Some(&first) = ip.first() else {
        return;
    };
    let header = usize::from(first & 0x0f) * 4;
    let total = ip
        .get(2..4)
        .map_or(ip.len(), |l| usize::from(u16::from_be_bytes([l[0], l[1]])))
        .min(ip.len());
    if total < header + 8 {
        return;
    }
    let protocol = ip[9];
    let echo = protocol == ICMP && matches!(ip[header], 0 | 8);
    for (end, at, port_at) in [(source, 12, 0), (destination, 16, 2)] {
        let Some((address, port)) = end else {
            continue;
        };
        ip[at..at + 4].copy_from_slice(&address.octets());
        let place = match protocol {
            TCP | UDP => Some(header + port_at),
            // A ping's identifier is the same field both ways.
            _ if echo => Some(header + 4),
            _ => None,
        };
        if let Some(place) = place {
            ip[place..place + 2].copy_from_slice(&port.to_be_bytes());
        }
    }
    ip[10..12].copy_from_slice(&[0, 0]);
    let sum = checksum(&ip[..header]);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    let (source, destination) = (ip_at(ip, 12), ip_at(ip, 16));
    let segment = &mut ip[header..total];
    let field = match protocol {
        TCP if segment.len() >= 20 => 16,
        UDP if segment[6..8] != [0, 0] => 6,
        ICMP => 2,
        _ => return,
    };
    segment[field..field + 2].copy_from_slice(&[0, 0]);
    let sum = if protocol == ICMP {
        checksum(segment)
    } else {
        let mut pseudo = Vec::with_capacity(12 + segment.len());
        pseudo.extend_from_slice(&source.octets());
        pseudo.extend_from_slice(&destination.octets());
        pseudo.extend_from_slice(&[0, protocol]);
        pseudo.extend_from_slice(&(segment.len() as u16).to_be_bytes());
        pseudo.extend_from_slice(segment);
        match checksum(&pseudo) {
            0 if protocol == UDP => 0xffff,
            sum => sum,
        }
    };
    segment[field..field + 2].copy_from_slice(&sum.to_be_bytes());
}

/// One ARP message, the fields a switch cares about.
#[derive(Debug, Clone, Copy)]
pub struct Arp {
    /// 1 asks, 2 answers.
    pub operation: u16,
    /// Who asks.
    pub sender_mac: Mac,
    /// The address the sender claims.
    pub sender_ip: Ipv4Addr,
    /// The address asked about.
    pub target_ip: Ipv4Addr,
}

/// The ARP in `frame`, if it is Ethernet/IPv4 ARP.
pub fn arp(frame: &[u8]) -> Option<Arp> {
    let body = frame.get(14..42)?;
    if body[0..6] != [0, 1, 8, 0, 6, 4] {
        return None;
    }
    Some(Arp {
        operation: u16::from_be_bytes([body[6], body[7]]),
        sender_mac: body[8..14].try_into().ok()?,
        sender_ip: ip_at(body, 14),
        target_ip: ip_at(body, 24),
    })
}

/// "`ip` is at `mac`", said to whoever asked.
pub fn arp_reply(asked: &Arp, mac: Mac) -> Vec<u8> {
    let mut body = vec![0, 1, 8, 0, 6, 4, 0, 2];
    body.extend_from_slice(&mac);
    body.extend_from_slice(&asked.target_ip.octets());
    body.extend_from_slice(&asked.sender_mac);
    body.extend_from_slice(&asked.sender_ip.octets());
    ethernet(asked.sender_mac, mac, ETHER_ARP, &body)
}

fn ip_at(bytes: &[u8], at: usize) -> Ipv4Addr {
    Ipv4Addr::new(bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3])
}

/// What a firewall decides on: who, to whom, how, and which port.
///
/// For ICMP echo both ports are the echo's identifier, so a reply is the
/// [`Flow::reverse`] of its request, the way a TCP answer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Flow {
    /// Sender.
    pub source: IpAddr,
    /// Receiver.
    pub destination: IpAddr,
    /// [`ICMP`], [`ICMPV6`], [`TCP`], [`UDP`] or anything else IP carries.
    pub protocol: u8,
    /// The sender's port.
    pub source_port: u16,
    /// The receiver's port.
    pub destination_port: u16,
}

impl Flow {
    /// The sender, for what only speaks IPv4 (NAT, the way out): an IPv6
    /// sender is the unspecified address, which nothing matches.
    pub fn source4(self) -> Ipv4Addr {
        v4(self.source)
    }

    /// See [`Flow::source4`].
    pub fn destination4(self) -> Ipv4Addr {
        v4(self.destination)
    }

    /// Whether it is IPv6.
    pub fn is_v6(self) -> bool {
        self.source.is_ipv6()
    }

    /// The same conversation, the other way.
    pub fn reverse(self) -> Self {
        Self {
            source: self.destination,
            destination: self.source,
            protocol: self.protocol,
            source_port: self.destination_port,
            destination_port: self.source_port,
        }
    }
}

/// An IPv4 address, or the unspecified one for an IPv6 address.
pub fn v4(address: IpAddr) -> Ipv4Addr {
    match address {
        IpAddr::V4(address) => address,
        IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
    }
}

/// The IP protocols people call by name.
const PROTOCOL_NAMES: [(u8, &str); 11] = [
    (ICMP, "icmp"),
    (ICMPV6, "icmpv6"),
    (2, "igmp"),
    (TCP, "tcp"),
    (UDP, "udp"),
    (47, "gre"),
    (50, "esp"),
    (51, "ah"),
    (89, "ospf"),
    (112, "vrrp"),
    (132, "sctp"),
];

/// `tcp`, `udp`, `icmp`, `gre`..., or the number.
pub fn protocol_name(protocol: u8) -> String {
    PROTOCOL_NAMES
        .iter()
        .find(|(number, _)| *number == protocol)
        .map_or_else(|| protocol.to_string(), |(_, name)| (*name).to_owned())
}

/// The IP protocol `name` says: one of [`protocol_name`]'s names, or a number.
pub fn protocol_number(name: &str) -> Option<u8> {
    PROTOCOL_NAMES
        .iter()
        .find(|(_, known)| *known == name)
        .map(|(number, _)| *number)
        .or_else(|| name.parse().ok())
}

/// One IPv4 packet: its flow, and what it carries past the IP header.
#[derive(Debug)]
pub struct Ipv4<'a> {
    /// Who, to whom, how.
    pub flow: Flow,
    /// The transport header and what follows it.
    pub payload: &'a [u8],
}

/// The IPv4 packet in `frame`. A fragment after the first has no ports.
pub fn ipv4(frame: &[u8]) -> Option<Ipv4<'_>> {
    let ip = frame.get(14..)?;
    if ip.len() < 20 || ip[0] >> 4 != 4 {
        return None;
    }
    let header = usize::from(ip[0] & 0x0f) * 4;
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]])).min(ip.len());
    let payload = ip.get(header..total)?;
    let protocol = ip[9];
    let first_fragment = u16::from_be_bytes([ip[6], ip[7]]) & 0x1fff == 0;
    let (source_port, destination_port) = match protocol {
        TCP | UDP if first_fragment && payload.len() >= 4 => (
            u16::from_be_bytes([payload[0], payload[1]]),
            u16::from_be_bytes([payload[2], payload[3]]),
        ),
        ICMP if first_fragment && payload.len() >= 8 && matches!(payload[0], 0 | 8) => {
            let id = u16::from_be_bytes([payload[4], payload[5]]);
            (id, id)
        }
        _ => (0, 0),
    };
    Some(Ipv4 {
        flow: Flow {
            source: ip_at(ip, 12).into(),
            destination: ip_at(ip, 16).into(),
            protocol,
            source_port,
            destination_port,
        },
        payload,
    })
}

/// An IPv4 packet from `source` to `destination`, header checksum and all.
pub fn ipv4_packet(
    source: Ipv4Addr,
    destination: Ipv4Addr,
    protocol: u8,
    payload: &[u8],
) -> Vec<u8> {
    let total = (20 + payload.len()) as u16;
    let mut packet = vec![0x45, 0];
    packet.extend_from_slice(&total.to_be_bytes());
    packet.extend_from_slice(&[0, 0, 0x40, 0, 64, protocol, 0, 0]);
    packet.extend_from_slice(&source.octets());
    packet.extend_from_slice(&destination.octets());
    let sum = checksum(&packet);
    packet[10..12].copy_from_slice(&sum.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

/// A UDP datagram. The checksum is left at zero, which IPv4 allows.
pub fn udp(source_port: u16, destination_port: u16, data: &[u8]) -> Vec<u8> {
    let mut datagram = Vec::with_capacity(8 + data.len());
    datagram.extend_from_slice(&source_port.to_be_bytes());
    datagram.extend_from_slice(&destination_port.to_be_bytes());
    datagram.extend_from_slice(&((8 + data.len()) as u16).to_be_bytes());
    datagram.extend_from_slice(&[0, 0]);
    datagram.extend_from_slice(data);
    datagram
}

/// What a UDP datagram carries.
pub fn udp_data(payload: &[u8]) -> &[u8] {
    let length = payload
        .get(4..6)
        .map_or(0, |l| usize::from(u16::from_be_bytes([l[0], l[1]])));
    payload
        .get(8..length.clamp(8, payload.len()))
        .unwrap_or(&[])
}

/// The echo reply to an echo request's ICMP payload, or `None` if it is not
/// one.
pub fn echo_reply(icmp: &[u8]) -> Option<Vec<u8>> {
    if icmp.len() < 8 || icmp[0] != 8 {
        return None;
    }
    let mut reply = icmp.to_vec();
    reply[0] = 0;
    reply[2..4].copy_from_slice(&[0, 0]);
    let sum = checksum(&reply);
    reply[2..4].copy_from_slice(&sum.to_be_bytes());
    Some(reply)
}

/// One hop for an IPv4 packet a router carries on: its TTL one less, and
/// the header checksum again. `false`, untouched, when the TTL is spent.
pub fn hop(ip: &mut [u8]) -> bool {
    if ip.len() < 20 || ip[8] <= 1 {
        return false;
    }
    ip[8] -= 1;
    let header = usize::from(ip[0] & 0x0f) * 4;
    ip[10..12].copy_from_slice(&[0, 0]);
    let sum = checksum(&ip[..header.min(ip.len())]);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    true
}

/// `ip` sent with `ttl` hops to live; the header checksum again.
pub fn with_ttl(ip: &mut [u8], ttl: u8) {
    if ip.len() < 20 {
        return;
    }
    ip[8] = ttl;
    let header = usize::from(ip[0] & 0x0f) * 4;
    ip[10..12].copy_from_slice(&[0, 0]);
    let sum = checksum(&ip[..header.min(ip.len())]);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
}

/// An ICMP echo request: `ident` and `sequence`, and a little to echo.
pub fn echo_request(ident: u16, sequence: u16) -> Vec<u8> {
    let mut icmp = vec![8, 0, 0, 0];
    icmp.extend_from_slice(&ident.to_be_bytes());
    icmp.extend_from_slice(&sequence.to_be_bytes());
    icmp.extend_from_slice(b"BeatBox light machine ping 0123");
    let sum = checksum(&icmp);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());
    icmp
}

/// A DNS question for `name`'s IPv4 address, recursion wanted.
pub fn dns_query(id: u16, name: &str) -> Vec<u8> {
    dns_query_for(id, name, 1)
}

/// A DNS question for `name`, of `kind` (1 is A, 28 AAAA).
pub fn dns_query_for(id: u16, name: &str, kind: u16) -> Vec<u8> {
    let mut query = id.to_be_bytes().to_vec();
    query.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.trim_end_matches('.').split('.') {
        let label = &label.as_bytes()[..label.len().min(63)];
        query.push(label.len() as u8);
        query.extend_from_slice(label);
    }
    query.push(0);
    query.extend_from_slice(&kind.to_be_bytes());
    query.extend_from_slice(&[0, 1]);
    query
}

/// ICMP time exceeded.
pub const ICMP_TIME_EXCEEDED: u8 = 11;
/// ICMP destination unreachable.
pub const ICMP_UNREACHABLE: u8 = 3;

/// An ICMP error about `original`, the packet that caused it: its header
/// and the first eight bytes after, as RFC 792 asks.
pub fn icmp_error(kind: u8, code: u8, original: &[u8]) -> Vec<u8> {
    let header = original.first().map_or(20, |b| usize::from(b & 0x0f) * 4);
    let quoted = &original[..original.len().min(header + 8)];
    let mut icmp = vec![kind, code, 0, 0, 0, 0, 0, 0];
    icmp.extend_from_slice(quoted);
    let sum = checksum(&icmp);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());
    icmp
}

/// DHCP message types.
pub const DHCP_DISCOVER: u8 = 1;
/// See [`DHCP_DISCOVER`].
pub const DHCP_OFFER: u8 = 2;
/// See [`DHCP_DISCOVER`].
pub const DHCP_REQUEST: u8 = 3;
/// See [`DHCP_DISCOVER`].
pub const DHCP_ACK: u8 = 5;
/// See [`DHCP_DISCOVER`].
pub const DHCP_NAK: u8 = 6;

const COOKIE: [u8; 4] = [99, 130, 83, 99];

/// Default domain for networks (`dns.domain`) and the Server's DNS.
///
/// `.local` is normally mDNS on Ubuntu, which never crosses our networks.
/// Handing it out via DHCP makes systemd-resolved ask our DNS, and the SOA in
/// [`dns_answer`] makes nss-mdns back off (`local_soa`).
pub const DOMAIN: &str = "local";

/// A DHCP client's question.
#[derive(Debug, Clone, Copy)]
pub struct DhcpAsk {
    /// [`DHCP_DISCOVER`], [`DHCP_REQUEST`], ...
    pub kind: u8,
    xid: [u8; 4],
    flags: [u8; 2],
    /// Who asks.
    pub client: Mac,
    /// The address it would like, if it said, or the one it holds when it
    /// renews.
    pub requested: Option<Ipv4Addr>,
    /// The address it holds and renews (`ciaddr`): the answer goes there,
    /// unicast, as RFC 2131 §4.1 says.
    pub held: Option<Ipv4Addr>,
}

/// The DHCP question in a UDP datagram's data.
pub fn dhcp_ask(data: &[u8]) -> Option<DhcpAsk> {
    if data.len() < 240 || data[0] != 1 || data[236..240] != COOKIE {
        return None;
    }
    let mut kind = None;
    let mut requested = None;
    let mut options = &data[240..];
    while let [code, rest @ ..] = options {
        match *code {
            255 => break,
            0 => options = rest,
            _ => {
                let (&length, rest) = rest.split_first()?;
                let value = rest.get(..usize::from(length))?;
                match (*code, value) {
                    (53, [k]) => kind = Some(*k),
                    (50, [a, b, c, d]) => requested = Some(Ipv4Addr::new(*a, *b, *c, *d)),
                    _ => {}
                }
                options = &rest[usize::from(length)..];
            }
        }
    }
    // A renewal names its address in `ciaddr`, not in option 50.
    let held = Ipv4Addr::new(data[12], data[13], data[14], data[15]);
    let held = (!held.is_unspecified()).then_some(held);
    let requested = requested.or(held);
    Some(DhcpAsk {
        kind: kind?,
        xid: data[4..8].try_into().ok()?,
        flags: data[10..12].try_into().ok()?,
        client: data[28..34].try_into().ok()?,
        requested,
        held,
    })
}

/// What a DHCP answer hands out besides the address and the gateway.
#[derive(Debug, Clone, Copy)]
pub struct Offer<'a> {
    /// Seconds the address is held. Ten minutes unless set otherwise: a
    /// machine whose address moved (its port went to another VLAN) hears so
    /// when it renews or rebinds, without being restarted.
    pub lease: u32,
    /// Who answers names.
    pub dns: Ipv4Addr,
    /// The domain names are searched in.
    pub domain: &'a str,
}

/// The server's answer: `kind` of `address`, with the gateway as router
/// and server identifier, and the rest as `offer` says.
pub fn dhcp_answer(
    ask: &DhcpAsk,
    kind: u8,
    address: Ipv4Addr,
    gateway: Ipv4Addr,
    prefix: u8,
    offer: &Offer<'_>,
) -> Vec<u8> {
    let mut data = vec![0u8; 240];
    data[0..4].copy_from_slice(&[2, 1, 6, 0]);
    data[4..8].copy_from_slice(&ask.xid);
    data[10..12].copy_from_slice(&ask.flags);
    if kind != DHCP_NAK {
        data[16..20].copy_from_slice(&address.octets());
        data[20..24].copy_from_slice(&gateway.octets());
    }
    data[28..34].copy_from_slice(&ask.client);
    data[236..240].copy_from_slice(&COOKIE);
    data.extend_from_slice(&[53, 1, kind, 54, 4]);
    data.extend_from_slice(&gateway.octets());
    if kind != DHCP_NAK {
        let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
        let domain = &offer.domain.as_bytes()[..offer.domain.len().min(255)];
        data.extend_from_slice(&[51, 4]);
        data.extend_from_slice(&offer.lease.to_be_bytes());
        data.extend_from_slice(&[1, 4]);
        data.extend_from_slice(&mask.to_be_bytes());
        data.extend_from_slice(&[3, 4]);
        data.extend_from_slice(&gateway.octets());
        data.extend_from_slice(&[6, 4]);
        data.extend_from_slice(&offer.dns.octets());
        data.extend_from_slice(&[15, domain.len() as u8]);
        data.extend_from_slice(domain);
    }
    data.push(255);
    data.resize(data.len().max(300), 0);
    data
}

/// One DNS question.
#[derive(Debug, Clone)]
pub struct DnsQuestion<'a> {
    id: [u8; 2],
    recursion: u8,
    /// Lowercase, dotted, no trailing dot.
    pub name: String,
    /// 1 is A, 28 AAAA, and so on.
    pub kind: u16,
    question: &'a [u8],
}

/// The first question in a DNS query.
pub fn dns_question(data: &[u8]) -> Option<DnsQuestion<'_>> {
    if data.len() < 12 || data[2] & 0x80 != 0 || u16::from_be_bytes([data[4], data[5]]) == 0 {
        return None;
    }
    let mut at = 12;
    let mut labels = Vec::new();
    loop {
        let length = usize::from(*data.get(at)?);
        at += 1;
        if length == 0 {
            break;
        }
        if length > 63 {
            return None;
        }
        labels.push(String::from_utf8_lossy(data.get(at..at + length)?).to_ascii_lowercase());
        at += length;
    }
    let kind = u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]);
    Some(DnsQuestion {
        id: [data[0], data[1]],
        recursion: data[2] & 1,
        name: labels.join("."),
        kind,
        question: data.get(12..at + 4)?,
    })
}

/// From a resolver's answer: the name asked and its first IPv4 address,
/// if it had one. Names in the answers are skipped, compressed or not.
pub fn dns_first_a(data: &[u8]) -> Option<(String, Ipv4Addr)> {
    match dns_first(data)? {
        (name, IpAddr::V4(address)) => Some((name, address)),
        _ => None,
    }
}

/// From a resolver's answer: the name asked and its first address, IPv4
/// (A) or IPv6 (AAAA).
pub fn dns_first(data: &[u8]) -> Option<(String, IpAddr)> {
    if data.len() < 12 || data[2] & 0x80 == 0 {
        return None;
    }
    let answers = u16::from_be_bytes([data[6], data[7]]);
    // The question, as a query has it: same shape, its own parser.
    let mut query = data.to_vec();
    query[2] &= 0x7f;
    let question = dns_question(&query)?;
    let mut at = 12 + question.question.len();
    let skip_name = |mut at: usize| -> Option<usize> {
        loop {
            let length = *data.get(at)?;
            if length & 0xc0 == 0xc0 {
                return Some(at + 2);
            }
            at += 1 + usize::from(length);
            if length == 0 {
                return Some(at);
            }
        }
    };
    for _ in 0..answers {
        at = skip_name(at)?;
        let kind = u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]);
        let length = usize::from(u16::from_be_bytes([*data.get(at + 8)?, *data.get(at + 9)?]));
        let rdata = data.get(at + 10..at + 10 + length)?;
        if kind == 1 && length == 4 {
            return Some((
                question.name,
                Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]).into(),
            ));
        }
        if kind == 28 && length == 16 {
            return Some((question.name, ip6_at(rdata, 0).into()));
        }
        at += 10 + length;
    }
    None
}

/// A name in a DNS message at `at`, following pointers; and where the name
/// ends in place (after its pointer, if it had one).
fn read_name(data: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    // A pointer loop is a broken message, not a hang.
    for _ in 0..64 {
        let length = *data.get(at)?;
        if length & 0xc0 == 0xc0 {
            end.get_or_insert(at + 2);
            at = usize::from(u16::from_be_bytes([length & 0x3f, *data.get(at + 1)?]));
            continue;
        }
        if length == 0 {
            return Some((labels.join("."), end.unwrap_or(at + 1)));
        }
        let label = data.get(at + 1..at + 1 + usize::from(length))?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        at += 1 + usize::from(length);
    }
    None
}

/// Every answer in a resolver's reply as its type and what it says, read
/// the way `nslookup` prints it: an address, a name, `10 mail.lab`, text.
pub fn dns_records(data: &[u8]) -> Vec<(u16, String)> {
    let mut found = Vec::new();
    if data.len() < 12 || data[2] & 0x80 == 0 {
        return found;
    }
    let questions = u16::from_be_bytes([data[4], data[5]]);
    let answers = u16::from_be_bytes([data[6], data[7]]);
    let mut at = 12;
    for _ in 0..questions {
        let Some((_, next)) = read_name(data, at) else {
            return found;
        };
        at = next + 4;
    }
    for _ in 0..answers {
        let Some((_, next)) = read_name(data, at) else {
            break;
        };
        let (Some(kind), Some(length)) = (data.get(next..next + 2), data.get(next + 8..next + 10))
        else {
            break;
        };
        let kind = u16::from_be_bytes([kind[0], kind[1]]);
        let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
        let start = next + 10;
        let Some(rdata) = data.get(start..start + length) else {
            break;
        };
        let said = match (kind, length) {
            (1, 4) => Some(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]).to_string()),
            (28, 16) => Some(ip6_at(rdata, 0).to_string()),
            (5, _) => read_name(data, start).map(|(name, _)| name),
            (15, 3..) => read_name(data, start + 2)
                .map(|(name, _)| format!("{} {name}", u16::from_be_bytes([rdata[0], rdata[1]]))),
            (16, _) => {
                let mut text = String::new();
                let mut rest = rdata;
                while let [size, tail @ ..] = rest {
                    let size = usize::from(*size).min(tail.len());
                    text.push_str(&String::from_utf8_lossy(&tail[..size]));
                    rest = &tail[size..];
                }
                Some(text)
            }
            _ => None,
        };
        if let Some(said) = said {
            found.push((kind, said));
        }
        at = start + length;
    }
    found
}

/// Answer a question: `address` for a matching A/AAAA, empty for other types
/// on a known name, NXDOMAIN for unknown names. SOA for [`DOMAIN`] itself,
/// since we're its authority. Never asks upstream; there is none.
pub fn dns_answer(question: &DnsQuestion<'_>, known: bool, address: Option<IpAddr>) -> Vec<u8> {
    let answer = address.map(|address| {
        let record = match address {
            IpAddr::V4(a) => Record::A(a),
            IpAddr::V6(a) => Record::Aaaa(a),
        };
        (question.name.clone(), record)
    });
    let answers: Vec<_> = answer
        .into_iter()
        .filter(|(_, record)| record.kind() == question.kind)
        .collect();
    dns_reply(question, known, DOMAIN, &answers)
}

/// One record in a DNS answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// An IPv4 address.
    A(Ipv4Addr),
    /// An IPv6 address.
    Aaaa(Ipv6Addr),
    /// Another name for the same thing.
    Cname(String),
    /// Where mail for the name goes: a preference (lower first) and a name.
    Mx(u16, String),
    /// Text.
    Txt(String),
}

impl Record {
    /// Its type number: 1, 28, 5, 15 or 16.
    pub fn kind(&self) -> u16 {
        match self {
            Self::A(_) => 1,
            Self::Aaaa(_) => 28,
            Self::Cname(_) => 5,
            Self::Mx(..) => 15,
            Self::Txt(_) => 16,
        }
    }

    fn data(&self) -> Vec<u8> {
        match self {
            Self::A(a) => a.octets().to_vec(),
            Self::Aaaa(a) => a.octets().to_vec(),
            Self::Cname(name) => labels(name),
            Self::Mx(preference, name) => {
                let mut data = preference.to_be_bytes().to_vec();
                data.extend(labels(name));
                data
            }
            // Strings of up to 255 bytes, one after the other.
            Self::Txt(text) => text
                .as_bytes()
                .chunks(255)
                .flat_map(|chunk| std::iter::once(chunk.len() as u8).chain(chunk.iter().copied()))
                .collect(),
        }
    }
}

/// A name as DNS writes it: each label after its length, then a zero.
fn labels(name: &str) -> Vec<u8> {
    let mut data = Vec::new();
    for label in name
        .trim_end_matches('.')
        .split('.')
        .filter(|l| !l.is_empty())
    {
        let label = &label.as_bytes()[..label.len().min(63)];
        data.push(label.len() as u8);
        data.extend_from_slice(label);
    }
    data.push(0);
    data
}

/// The answer to `question`: `answers`, each with the name it is about, in
/// order (a CNAME before what it leads to). NOERROR for a name this network
/// `known`, NXDOMAIN otherwise. A SOA question about `domain` itself is
/// answered: this network is that zone's authority.
pub fn dns_reply(
    question: &DnsQuestion<'_>,
    known: bool,
    domain: &str,
    answers: &[(String, Record)],
) -> Vec<u8> {
    let soa = question.kind == 6 && question.name == domain;
    let count = answers.len().min(255) as u8 + u8::from(soa);
    let mut data = question.id.to_vec();
    data.push(0x84 | question.recursion);
    data.push(if known || soa { 0x80 } else { 0x83 });
    data.extend_from_slice(&[0, 1, 0, count, 0, 0, 0, 0]);
    data.extend_from_slice(question.question);
    for (owner, record) in answers.iter().take(255) {
        // The name asked is at offset 12 already; any other is written out.
        if *owner == question.name {
            data.extend_from_slice(&[0xc0, 12]);
        } else {
            data.extend(labels(owner));
        }
        let rdata = record.data();
        data.extend_from_slice(&record.kind().to_be_bytes());
        data.extend_from_slice(&[0, 1, 0, 0, 0, 60]);
        data.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        data.extend(rdata);
    }
    if soa {
        // gateway.local. hostmaster.local. 1 3600 600 86400 60
        let mut record = vec![7];
        record.extend_from_slice(b"gateway\xc0\x0c\x0ahostmaster\xc0\x0c");
        for number in [1u32, 3600, 600, 86_400, 60] {
            record.extend_from_slice(&number.to_be_bytes());
        }
        data.extend_from_slice(&[0xc0, 12, 0, 6, 0, 1, 0, 0, 0, 60]);
        data.extend_from_slice(&(record.len() as u16).to_be_bytes());
        data.extend_from_slice(&record);
    }
    data
}

/// One IPv6 packet: its flow, its hop limit, and what it carries past the
/// fixed header.
/// ponytail: extension headers are not walked; a packet with one is told by
/// the first next-header, with no ports.
#[derive(Debug)]
pub struct Ipv6<'a> {
    /// Who, to whom, how.
    pub flow: Flow,
    /// Hops left.
    pub hop_limit: u8,
    /// The transport header and what follows it.
    pub payload: &'a [u8],
}

fn ip6_at(bytes: &[u8], at: usize) -> Ipv6Addr {
    let octets: [u8; 16] = bytes[at..at + 16].try_into().expect("sixteen bytes");
    Ipv6Addr::from(octets)
}

/// The IPv6 packet in `frame`.
pub fn ipv6(frame: &[u8]) -> Option<Ipv6<'_>> {
    let ip = frame.get(14..)?;
    if ip.len() < 40 || ip[0] >> 4 != 6 {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([ip[4], ip[5]]));
    let payload = ip.get(40..(40 + length).min(ip.len()))?;
    let protocol = ip[6];
    let (source_port, destination_port) = match protocol {
        TCP | UDP if payload.len() >= 4 => (
            u16::from_be_bytes([payload[0], payload[1]]),
            u16::from_be_bytes([payload[2], payload[3]]),
        ),
        ICMPV6 if payload.len() >= 8 && matches!(payload[0], ECHO6_REQUEST | ECHO6_REPLY) => {
            let id = u16::from_be_bytes([payload[4], payload[5]]);
            (id, id)
        }
        _ => (0, 0),
    };
    Some(Ipv6 {
        flow: Flow {
            source: ip6_at(ip, 8).into(),
            destination: ip6_at(ip, 24).into(),
            protocol,
            source_port,
            destination_port,
        },
        hop_limit: ip[7],
        payload,
    })
}

/// An IPv6 packet from `source` to `destination`.
pub fn ipv6_packet(
    source: Ipv6Addr,
    destination: Ipv6Addr,
    protocol: u8,
    hop_limit: u8,
    payload: &[u8],
) -> Vec<u8> {
    let mut packet = vec![0x60, 0, 0, 0];
    packet.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    packet.extend_from_slice(&[protocol, hop_limit]);
    packet.extend_from_slice(&source.octets());
    packet.extend_from_slice(&destination.octets());
    packet.extend_from_slice(payload);
    packet
}

/// The checksum of an upper-layer message over IPv6: with the pseudo-header
/// IPv6 asks for, which ICMPv6 needs too.
pub fn checksum6(source: Ipv6Addr, destination: Ipv6Addr, protocol: u8, message: &[u8]) -> u16 {
    let mut pseudo = Vec::with_capacity(40 + message.len());
    pseudo.extend_from_slice(&source.octets());
    pseudo.extend_from_slice(&destination.octets());
    pseudo.extend_from_slice(&(message.len() as u32).to_be_bytes());
    pseudo.extend_from_slice(&[0, 0, 0, protocol]);
    pseudo.extend_from_slice(message);
    checksum(&pseudo)
}

/// An ICMPv6 message in a whole packet, its checksum made.
pub fn icmpv6_packet(
    source: Ipv6Addr,
    destination: Ipv6Addr,
    hop_limit: u8,
    mut message: Vec<u8>,
) -> Vec<u8> {
    message[2..4].copy_from_slice(&[0, 0]);
    let sum = checksum6(source, destination, ICMPV6, &message);
    message[2..4].copy_from_slice(&sum.to_be_bytes());
    ipv6_packet(source, destination, ICMPV6, hop_limit, &message)
}

/// ICMPv6 message types.
pub const ICMPV6_UNREACHABLE: u8 = 1;
/// See [`ICMPV6_UNREACHABLE`].
pub const ICMPV6_TIME_EXCEEDED: u8 = 3;
/// See [`ICMPV6_UNREACHABLE`].
pub const ECHO6_REQUEST: u8 = 128;
/// See [`ICMPV6_UNREACHABLE`].
pub const ECHO6_REPLY: u8 = 129;
/// Neighbour discovery: "is there a router?"
pub const ROUTER_SOLICIT: u8 = 133;
/// Neighbour discovery: "I am a router, and this is the network."
pub const ROUTER_ADVERT: u8 = 134;
/// Neighbour discovery: "who has this address?"
pub const NEIGHBOR_SOLICIT: u8 = 135;
/// Neighbour discovery: "I have it, at this MAC."
pub const NEIGHBOR_ADVERT: u8 = 136;
/// Neighbour discovery: "go by that router instead."
pub const REDIRECT: u8 = 137;

/// Whether an ICMPv6 type is neighbour discovery: what IPv6 needs to work
/// at all, and no firewall rule's business.
pub fn is_discovery(kind: u8) -> bool {
    (ROUTER_SOLICIT..=REDIRECT).contains(&kind)
}

/// All the nodes on a link.
pub const ALL_NODES: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);

/// The Ethernet address an IPv6 multicast address goes to.
pub fn multicast_mac(address: Ipv6Addr) -> Mac {
    let o = address.octets();
    [0x33, 0x33, o[12], o[13], o[14], o[15]]
}

/// An address made of a /64 prefix and a MAC (EUI-64), the way SLAAC makes
/// one: fe80::/64 gives the link-local address.
pub fn eui64(prefix: Ipv6Addr, mac: Mac) -> Ipv6Addr {
    let mut o = prefix.octets();
    o[8..16].copy_from_slice(&[
        mac[0] ^ 2,
        mac[1],
        mac[2],
        0xff,
        0xfe,
        mac[3],
        mac[4],
        mac[5],
    ]);
    Ipv6Addr::from(o)
}

/// `fe80::/64` with `mac`'s interface identifier.
pub fn link_local(mac: Mac) -> Ipv6Addr {
    eui64(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), mac)
}

/// What an ICMPv6 neighbour solicitation asks about.
pub fn solicited(icmp: &[u8]) -> Option<Ipv6Addr> {
    (icmp.len() >= 24 && icmp[0] == NEIGHBOR_SOLICIT).then(|| ip6_at(icmp, 8))
}

/// "`target` is at `mac`": a router's neighbour advertisement.
pub fn neighbor_advert(target: Ipv6Addr, mac: Mac) -> Vec<u8> {
    // Router, solicited, override.
    let mut message = vec![NEIGHBOR_ADVERT, 0, 0, 0, 0xe0, 0, 0, 0];
    message.extend_from_slice(&target.octets());
    message.extend_from_slice(&[2, 1]);
    message.extend_from_slice(&mac);
    message
}

/// A router advertisement for one /64 the machines make their own address
/// in (SLAAC), from a router at `mac`.
pub fn router_advert(prefix: Ipv6Addr, mac: Mac) -> Vec<u8> {
    // Hop limit 64, no DHCPv6, a router for 30 minutes.
    let mut message = vec![
        ROUTER_ADVERT,
        0,
        0,
        0,
        64,
        0,
        0x07,
        0x08,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    message.extend_from_slice(&[1, 1]);
    message.extend_from_slice(&mac);
    // On-link and autonomous; valid a day, preferred four hours.
    message.extend_from_slice(&[3, 4, 64, 0xc0]);
    message.extend_from_slice(&86_400u32.to_be_bytes());
    message.extend_from_slice(&14_400u32.to_be_bytes());
    message.extend_from_slice(&[0, 0, 0, 0]);
    message.extend_from_slice(&prefix.octets());
    message
}

/// The prefix a router advertisement offers, if it offers one.
pub fn advertised(icmp: &[u8]) -> Option<Ipv6Addr> {
    if icmp.first() != Some(&ROUTER_ADVERT) {
        return None;
    }
    let mut at = 16;
    while let (Some(&kind), Some(&length)) = (icmp.get(at), icmp.get(at + 1)) {
        let length = usize::from(length) * 8;
        if length == 0 {
            return None;
        }
        if kind == 3 && length == 32 {
            return icmp.get(at + 16..at + 32).map(|_| ip6_at(icmp, at + 16));
        }
        at += length;
    }
    None
}

/// The echo reply to an ICMPv6 echo request, before its checksum
/// ([`icmpv6_packet`] makes it).
pub fn echo6_reply(icmp: &[u8]) -> Option<Vec<u8>> {
    if icmp.len() < 8 || icmp[0] != ECHO6_REQUEST {
        return None;
    }
    let mut reply = icmp.to_vec();
    reply[0] = ECHO6_REPLY;
    Some(reply)
}

/// An ICMPv6 echo request, before its checksum.
pub fn echo6_request(ident: u16, sequence: u16) -> Vec<u8> {
    let mut icmp = vec![ECHO6_REQUEST, 0, 0, 0];
    icmp.extend_from_slice(&ident.to_be_bytes());
    icmp.extend_from_slice(&sequence.to_be_bytes());
    icmp.extend_from_slice(b"BeatBox light machine ping 0123");
    icmp
}

/// An ICMPv6 error about `original`, before its checksum: as much of the
/// packet as fits the minimum MTU, as RFC 4443 asks.
pub fn icmpv6_error(kind: u8, code: u8, original: &[u8]) -> Vec<u8> {
    let mut icmp = vec![kind, code, 0, 0, 0, 0, 0, 0];
    icmp.extend_from_slice(&original[..original.len().min(1232)]);
    icmp
}

/// One hop for an IPv6 packet a router carries on: its hop limit one less.
/// `false`, untouched, when it is spent.
pub fn hop6(ip: &mut [u8]) -> bool {
    if ip.len() < 40 || ip[7] <= 1 {
        return false;
    }
    ip[7] -= 1;
    true
}

/// The server name a TLS ClientHello says (SNI), from the first bytes of
/// a connection's data.
pub fn tls_server_name(data: &[u8]) -> Option<String> {
    // A handshake record, a ClientHello in it.
    if data.first() != Some(&0x16) || data.get(5) != Some(&1) {
        return None;
    }
    let u16_at = |at: usize| {
        Some(usize::from(u16::from_be_bytes([
            *data.get(at)?,
            *data.get(at + 1)?,
        ])))
    };
    // Record header 5, handshake header 4, version 2, random 32.
    let mut at = 5 + 4 + 2 + 32;
    at += 1 + usize::from(*data.get(at)?); // session id
    at += 2 + u16_at(at)?; // cipher suites
    at += 1 + usize::from(*data.get(at)?); // compression methods
    let end = (at + 2 + u16_at(at)?).min(data.len());
    at += 2;
    while at + 4 <= end {
        let (kind, length) = (u16_at(at)?, u16_at(at + 2)?);
        if kind == 0 {
            // server_name: a list, its first entry a host name.
            let name_length = u16_at(at + 7)?;
            let name = data.get(at + 9..at + 9 + name_length)?;
            return Some(String::from_utf8_lossy(name).to_ascii_lowercase());
        }
        at += 4 + length;
    }
    None
}

/// The host an HTTP request is for, without its port.
pub fn http_host(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data.get(..data.len().min(2048))?).ok()?;
    let (first, rest) = text.split_once("\r\n")?;
    if !first.ends_with("HTTP/1.0") && !first.ends_with("HTTP/1.1") {
        return None;
    }
    rest.split("\r\n").find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case("host").then(|| {
            let host = value.trim();
            let host = host
                .rsplit_once(':')
                .filter(|(_, p)| p.parse::<u16>().is_ok())
                .map_or(host, |(h, _)| h);
            host.to_ascii_lowercase()
        })
    })
}

/// A TCP reset answering `ip`, a TCP segment that carries data, as from
/// where it was going: what cuts a connection already open. `None` for
/// anything else.
pub fn tcp_reset(ip: &[u8]) -> Option<Vec<u8>> {
    let v6 = ip.first()? >> 4 == 6;
    let (header, protocol) = if v6 {
        (40, *ip.get(6)?)
    } else {
        (usize::from(ip[0] & 0x0f) * 4, *ip.get(9)?)
    };
    if protocol != TCP {
        return None;
    }
    let total = if v6 {
        40 + usize::from(u16::from_be_bytes([*ip.get(4)?, *ip.get(5)?]))
    } else {
        usize::from(u16::from_be_bytes([*ip.get(2)?, *ip.get(3)?]))
    }
    .min(ip.len());
    let segment = ip.get(header..total)?;
    let offset = usize::from(*segment.get(12)? >> 4) * 4;
    let carried = segment.len().checked_sub(offset)?;
    if carried == 0 {
        return None;
    }
    let seq = u32::from_be_bytes(segment.get(4..8)?.try_into().ok()?);
    let ack = u32::from_be_bytes(segment.get(8..12)?.try_into().ok()?);
    let mut reset = Vec::with_capacity(20);
    reset.extend_from_slice(&segment[2..4]);
    reset.extend_from_slice(&segment[0..2]);
    reset.extend_from_slice(&ack.to_be_bytes());
    reset.extend_from_slice(&seq.wrapping_add(carried as u32).to_be_bytes());
    // Twenty bytes, RST and ACK, no window, checksum after.
    reset.extend_from_slice(&[0x50, 0x14, 0, 0, 0, 0, 0, 0]);
    if v6 {
        let from = ip6_at(ip, 24);
        let to = ip6_at(ip, 8);
        let sum = checksum6(from, to, TCP, &reset);
        reset[16..18].copy_from_slice(&sum.to_be_bytes());
        Some(ipv6_packet(from, to, TCP, 64, &reset))
    } else {
        let (from, to) = (ip_at(ip, 16), ip_at(ip, 12));
        let mut pseudo = [from.octets(), to.octets()].concat();
        pseudo.extend_from_slice(&[0, TCP, 0, 20]);
        pseudo.extend_from_slice(&reset);
        let sum = checksum(&pseudo);
        reset[16..18].copy_from_slice(&sum.to_be_bytes());
        Some(ipv4_packet(from, to, TCP, &reset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rewritten_packet_has_right_checksums() {
        let (a, b, c) = (
            Ipv4Addr::new(10, 0, 1, 10),
            Ipv4Addr::new(10, 0, 0, 11),
            Ipv4Addr::new(10, 0, 100, 2),
        );
        let mut tcp = vec![
            0x9c, 0x40, 0, 80, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0, 0,
        ];
        let mut pseudo = [a.octets(), b.octets()].concat();
        pseudo.extend_from_slice(&[0, TCP, 0, 20]);
        pseudo.extend_from_slice(&tcp);
        tcp[16..18].copy_from_slice(&checksum(&pseudo).to_be_bytes());
        let mut ip = ipv4_packet(a, b, TCP, &tcp);
        rewrite(&mut ip, Some((c, 40001)), None);
        assert_eq!(checksum(&ip[..20]), 0, "header");
        let mut again = [c.octets(), b.octets()].concat();
        again.extend_from_slice(&[0, TCP, 0, 20]);
        again.extend_from_slice(&ip[20..]);
        assert_eq!(checksum(&again), 0, "segment");
        let frame = ethernet(BROADCAST, BROADCAST, ETHER_IPV4, &ip);
        assert_eq!(ipv4(&frame).unwrap().flow.source_port, 40001);
        // A ping's identifier, and its ICMP checksum.
        let mut ping = ipv4_packet(a, b, ICMP, &echo_request(7, 1));
        rewrite(&mut ping, Some((c, 9)), None);
        assert_eq!(checksum(&ping[20..]), 0);
        assert_eq!(&ping[24..26], &9u16.to_be_bytes());
    }

    #[test]
    fn an_ipv6_echo_reads_back_and_its_checksum_holds() {
        let mac = [0x02, 0xbb, 0x0a, 0x32, 0x14, 0x0b];
        let me = link_local(mac);
        assert_eq!(me, "fe80::bb:aff:fe32:140b".parse::<Ipv6Addr>().unwrap());
        let to = eui64("fd00:10:70:1::".parse().unwrap(), mac);
        let ip = icmpv6_packet(me, to, 64, echo6_request(7, 1));
        let frame = ethernet(BROADCAST, mac, ETHER_IPV6, &ip);
        let packet = ipv6(&frame).unwrap();
        assert_eq!(
            (packet.flow.source, packet.flow.destination),
            (me.into(), to.into())
        );
        assert_eq!(
            (
                packet.flow.protocol,
                packet.flow.source_port,
                packet.hop_limit
            ),
            (ICMPV6, 7, 64)
        );
        assert_eq!(checksum6(me, to, ICMPV6, packet.payload), 0);
    }

    #[test]
    fn a_connection_says_its_name_and_is_cut_with_a_reset() {
        let request = b"GET / HTTP/1.0\r\nHost: Portal.Local:8080\r\nUser-Agent: x\r\n\r\n";
        assert_eq!(http_host(request).as_deref(), Some("portal.local"));
        // A ClientHello for youtube.com: record, handshake, version, random,
        // no session, one suite, one compression, the server_name extension.
        let name = b"youtube.com";
        let mut sni = vec![0, 0];
        sni.extend_from_slice(&((name.len() + 3) as u16).to_be_bytes());
        sni.extend_from_slice(&((name.len() + 3) as u16).to_be_bytes());
        sni.push(0);
        sni.extend_from_slice(&(name.len() as u16).to_be_bytes());
        sni.extend_from_slice(name);
        sni[2..4].copy_from_slice(&((name.len() + 5) as u16).to_be_bytes());
        let mut body = vec![3, 3];
        body.extend_from_slice(&[0; 32]);
        body.extend_from_slice(&[0, 0, 2, 0x13, 0x01, 1, 0]);
        body.extend_from_slice(&(sni.len() as u16).to_be_bytes());
        body.extend_from_slice(&sni);
        let mut hello = vec![0x16, 3, 1, 0, 0, 1, 0, 0, body.len() as u8];
        hello.extend_from_slice(&body);
        assert_eq!(tls_server_name(&hello).as_deref(), Some("youtube.com"));
        assert_eq!(http_host(&hello), None);
        // A segment with that data, and the reset that answers it.
        let (a, b) = (Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 2));
        let mut segment = vec![
            0x9c, 0x40, 1, 187, 0, 0, 0, 10, 0, 0, 0, 77, 0x50, 0x18, 0xff, 0xff, 0, 0, 0, 0,
        ];
        segment.extend_from_slice(request);
        let ip = ipv4_packet(a, b, TCP, &segment);
        let reset = tcp_reset(&ip).unwrap();
        let frame = ethernet(BROADCAST, BROADCAST, ETHER_IPV4, &reset);
        let flow = ipv4(&frame).unwrap().flow;
        assert_eq!(
            (
                flow.source,
                flow.destination,
                flow.source_port,
                flow.destination_port
            ),
            (b.into(), a.into(), 443, 0x9c40)
        );
        assert_eq!(
            &reset[24..28],
            &77u32.to_be_bytes(),
            "its sequence is the ack it was sent"
        );
        assert_eq!(&reset[28..32], &(10 + request.len() as u32).to_be_bytes());
        let mut pseudo = [b.octets(), a.octets()].concat();
        pseudo.extend_from_slice(&[0, TCP, 0, 20]);
        pseudo.extend_from_slice(&reset[20..]);
        assert_eq!(checksum(&pseudo), 0);
        // A bare SYN carries nothing: no reset for it.
        assert!(tcp_reset(&ipv4_packet(a, b, TCP, &segment[..20])).is_none());
    }

    #[test]
    fn records_are_written_and_read_back() {
        let ask = |name: &str, kind| dns_query_for(7, name, kind);
        let query = ask("www.lab", 1);
        let question = dns_question(&query).expect("a question");
        let reply = dns_reply(
            &question,
            true,
            "lab",
            &[
                ("www.lab".into(), Record::Cname("intranet.lab".into())),
                (
                    "intranet.lab".into(),
                    Record::A(Ipv4Addr::new(10, 20, 30, 11)),
                ),
            ],
        );
        assert_eq!(
            dns_records(&reply),
            [(5, "intranet.lab".into()), (1, "10.20.30.11".into())]
        );
        // What reads the first address still finds it past the CNAME.
        assert_eq!(
            dns_first_a(&reply).map(|(_, a)| a),
            Some(Ipv4Addr::new(10, 20, 30, 11))
        );

        let query = ask("lab", 15);
        let question = dns_question(&query).expect("a question");
        let long = "x".repeat(300);
        let reply = dns_reply(
            &question,
            true,
            "lab",
            &[
                ("lab".into(), Record::Mx(10, "correio.lab".into())),
                ("lab".into(), Record::Txt(long.clone())),
            ],
        );
        assert_eq!(
            dns_records(&reply),
            [(15, "10 correio.lab".into()), (16, long)]
        );

        // Nothing for a name nobody has: NXDOMAIN, and no answers.
        let query = ask("nada.lab", 1);
        let reply = dns_reply(
            &dns_question(&query).expect("a question"),
            false,
            "lab",
            &[],
        );
        assert_eq!(reply[3] & 0x0f, 3);
        assert!(dns_records(&reply).is_empty());
    }
}
