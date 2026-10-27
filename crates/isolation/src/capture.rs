//! What the guest tried to say, watched from outside it.
//!
//! Cheap and hard to evade: the host owns the virtual NIC, so QEMU's
//! `filter-dump` sees every packet with nothing in the guest and no root.
//!
//! **Attempts, not successes.** On an `isolated` machine SLIRP drops what
//! goes out, but it passes the dump first, so a contained sample still shows
//! who it tried to phone.
//!
//! Hand-written because it parses bytes a sample produced. It never trusts a
//! length past the buffer and gives up on a record rather than failing. Its
//! test checks the same bytes against the real `tshark`.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

/// pcap's link type for Ethernet, which is what `filter-dump` writes.
pub const LINKTYPE_ETHERNET: u32 = 1;

const MAGIC: u32 = 0xA1B2_C3D4;
const MAGIC_NANOSECONDS: u32 = 0xA1B2_3C4D;

/// A file that is not a capture this can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError(pub String);

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CaptureError {}

/// One destination the guest talked to, and how much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// `TCP`, `UDP`, `ICMP`, `ARP`, or the protocol number.
    pub protocol: String,
    /// Where it was going.
    pub destination: String,
    /// Which port, when the protocol has one.
    pub port: u16,
    /// How many frames.
    pub packets: u64,
    /// How many bytes of frame.
    pub bytes: u64,
}

impl Conversation {
    /// `93.184.216.34:443`, or the address alone when there is no port.
    pub fn endpoint(&self) -> String {
        if self.port != 0 {
            format!("{}:{}", self.destination, self.port)
        } else {
            self.destination.clone()
        }
    }
}

/// What a run's traffic amounted to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaptureSummary {
    /// Busiest first.
    pub conversations: Vec<Conversation>,
    /// The names it looked up, in the order it first asked.
    pub dns_queries: Vec<String>,
    /// Every frame read.
    pub packets: u64,
    /// Frames and records that could not be read.
    pub unreadable_records: u64,
}

impl CaptureSummary {
    /// Every destination, in the order of the conversation list.
    pub fn contacted(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for conversation in &self.conversations {
            if !seen.contains(&conversation.destination) {
                seen.push(conversation.destination.clone());
            }
        }
        seen
    }

    /// Whether the guest said nothing at all.
    pub fn silent(&self) -> bool {
        self.conversations.is_empty() && self.dns_queries.is_empty()
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// The name a DNS query asked about, if this looks like one. Only the first
/// question and only the labels; anything compressed or malformed is empty
/// rather than a guess.
fn dns_question(payload: &[u8]) -> String {
    if payload.len() < 13 {
        return String::new();
    }
    let Some(flags) = u16_at(payload, 2) else {
        return String::new();
    };
    if flags & 0x8000 != 0 {
        return String::new(); // a response, not a question
    }
    let mut labels = Vec::new();
    let mut offset = 12;
    while offset < payload.len() {
        let length = payload[offset] as usize;
        if length == 0 {
            break;
        }
        // 0xC0 marks a compression pointer: skipped, not followed.
        if length >= 0xC0 || offset + 1 + length > payload.len() {
            return String::new();
        }
        labels
            .push(String::from_utf8_lossy(&payload[offset + 1..offset + 1 + length]).into_owned());
        offset += 1 + length;
    }
    labels.join(".")
}

/// Ethernet -> IP -> TCP/UDP, or nothing at all: `(protocol, destination,
/// port, payload)`. Every length is checked against the buffer actually
/// held.
fn parse_frame(frame: &[u8]) -> Option<(String, String, u16, &[u8])> {
    if frame.len() < 14 {
        return None;
    }
    let ethertype = u16_at(frame, 12)?;
    let (protocol, destination, rest) = match ethertype {
        0x0800 => {
            if frame.len() < 34 {
                return None;
            }
            let header = usize::from(frame[14] & 0x0F) * 4;
            if header < 20 || frame.len() < 14 + header {
                return None;
            }
            let address: [u8; 4] = frame[30..34].try_into().ok()?;
            (
                frame[23],
                Ipv4Addr::from(address).to_string(),
                &frame[14 + header..],
            )
        }
        0x86DD => {
            if frame.len() < 54 {
                return None;
            }
            let address: [u8; 16] = frame[38..54].try_into().ok()?;
            (frame[20], Ipv6Addr::from(address).to_string(), &frame[54..])
        }
        0x0806 => return Some(("ARP".into(), "broadcast".into(), 0, &[])),
        _ => return None,
    };
    let name = match protocol {
        6 => "TCP".to_owned(),
        17 => "UDP".to_owned(),
        1 => "ICMP".to_owned(),
        other => other.to_string(),
    };
    if matches!(protocol, 6 | 17) && rest.len() >= 4 {
        let port = u16_at(rest, 2)?;
        let offset = if protocol == 17 {
            8
        } else if rest.len() >= 13 {
            usize::from(rest[12] >> 4) * 4
        } else {
            20
        };
        let payload = if rest.len() > offset {
            &rest[offset..]
        } else {
            &[]
        };
        return Some((name, destination, port, payload));
    }
    Some((name, destination, 0, &[]))
}

/// Summarise a pcap into who was contacted, and what was looked up.
pub fn read_capture(path: &Path) -> Result<CaptureSummary, CaptureError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let data = fs::read(path).map_err(|_| CaptureError(format!("{name} could not be read.")))?;
    read_bytes(&data, &name)
}

/// [`read_capture`], on bytes already held.
pub fn read_bytes(data: &[u8], name: &str) -> Result<CaptureSummary, CaptureError> {
    if data.len() < 24 {
        // QEMU creates the file as soon as the machine starts, so an empty
        // one is the normal state of a guest that has not spoken yet.
        return Ok(CaptureSummary::default());
    }
    let little = u32::from_le_bytes(data[0..4].try_into().expect("four bytes"));
    let big = u32::from_be_bytes(data[0..4].try_into().expect("four bytes"));
    let read_u32: fn(&[u8]) -> u32 = if little == MAGIC || little == MAGIC_NANOSECONDS {
        |b| u32::from_le_bytes(b.try_into().expect("four bytes"))
    } else if big == MAGIC || big == MAGIC_NANOSECONDS {
        |b| u32::from_be_bytes(b.try_into().expect("four bytes"))
    } else {
        return Err(CaptureError(format!("{name} is not a pcap file.")));
    };
    let link_type = read_u32(&data[20..24]);
    if link_type != LINKTYPE_ETHERNET {
        return Err(CaptureError(format!(
            "{name} holds link type {link_type}; only Ethernet captures are read."
        )));
    }

    let mut summary = CaptureSummary::default();
    let mut counts: HashMap<(String, String, u16), (u64, u64)> = HashMap::new();
    let mut offset = 24;
    while offset + 16 <= data.len() {
        let included = read_u32(&data[offset + 8..offset + 12]) as usize;
        // What was on the wire, not what was kept: a capture cut at a
        // snapshot length still counts every byte the guest sent.
        let original = u64::from(read_u32(&data[offset + 12..offset + 16]));
        offset += 16;
        if included == 0 || offset + included > data.len() {
            // A truncated final record is what a capture from a machine still
            // running looks like; it is not an error.
            summary.unreadable_records += 1;
            break;
        }
        let frame = &data[offset..offset + included];
        offset += included;
        summary.packets += 1;

        let Some((protocol, destination, port, payload)) = parse_frame(frame) else {
            summary.unreadable_records += 1;
            continue;
        };
        if protocol == "UDP" && port == 53 && !payload.is_empty() {
            let asked = dns_question(payload);
            if !asked.is_empty() && !summary.dns_queries.contains(&asked) {
                summary.dns_queries.push(asked);
            }
        }
        let entry = counts.entry((protocol, destination, port)).or_default();
        entry.0 += 1;
        entry.1 += original.max(frame.len() as u64);
    }

    let mut conversations: Vec<Conversation> = counts
        .into_iter()
        .map(
            |((protocol, destination, port), (packets, bytes))| Conversation {
                protocol,
                destination,
                port,
                packets,
                bytes,
            },
        )
        .collect();
    // Busiest first, then destination, protocol and port, so the order
    // doesn't depend on the map.
    conversations.sort_by(|a, b| {
        b.packets
            .cmp(&a.packets)
            .then_with(|| a.destination.cmp(&b.destination))
            .then_with(|| a.protocol.cmp(&b.protocol))
            .then_with(|| a.port.cmp(&b.port))
    });
    summary.conversations = conversations;
    Ok(summary)
}
