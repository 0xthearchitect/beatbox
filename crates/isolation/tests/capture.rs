//! Reading what the guest tried to say.
//!
//! The bytes come from whatever ran in the sandbox, so half of these feed it
//! truncated records, lying lengths and unknown frames. Two check against the
//! real `tshark` where it's installed.

mod support;

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use beatbox_isolation::capture::{LINKTYPE_ETHERNET, read_capture};
use support::Room;

fn pcap(frames: &[Vec<u8>], big_endian: bool, link_type: u32) -> Vec<u8> {
    let u32b = |v: u32| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let u16b = |v: u16| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let mut out = Vec::new();
    out.extend(u32b(0xA1B2_C3D4));
    out.extend(u16b(2));
    out.extend(u16b(4));
    out.extend(u32b(0));
    out.extend(u32b(0));
    out.extend(u32b(65535));
    out.extend(u32b(link_type));
    for frame in frames {
        out.extend(u32b(0));
        out.extend(u32b(0));
        out.extend(u32b(frame.len() as u32));
        out.extend(u32b(frame.len() as u32));
        out.extend(frame);
    }
    out
}

fn little(frames: &[Vec<u8>]) -> Vec<u8> {
    pcap(frames, false, LINKTYPE_ETHERNET)
}

fn ethernet(ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0xaa; 6];
    frame.extend([0xbb; 6]);
    frame.extend(ethertype.to_be_bytes());
    frame.extend(payload);
    frame
}

fn ipv4(protocol: u8, destination: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut header = vec![0x45, 0x00];
    header.extend((20 + payload.len() as u16).to_be_bytes());
    header.extend([0u8; 5]);
    header.push(protocol);
    header.extend([0u8; 2]);
    header.extend([10, 0, 2, 15]);
    header.extend(destination);
    header.extend(payload);
    ethernet(0x0800, &header)
}

fn tcp(port: u16, payload: &[u8]) -> Vec<u8> {
    let mut segment = Vec::new();
    segment.extend(40000u16.to_be_bytes());
    segment.extend(port.to_be_bytes());
    segment.extend([0u8; 8]);
    segment.extend([0x50, 0x02]);
    segment.extend([0u8; 6]);
    segment.extend(payload);
    segment
}

fn udp(port: u16, payload: &[u8]) -> Vec<u8> {
    let mut datagram = Vec::new();
    datagram.extend(40000u16.to_be_bytes());
    datagram.extend(port.to_be_bytes());
    datagram.extend((8 + payload.len() as u16).to_be_bytes());
    datagram.extend([0u8; 2]);
    datagram.extend(payload);
    datagram
}

fn dns_query(name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for field in [0x1234u16, 0x0100, 1, 0, 0, 0] {
        body.extend(field.to_be_bytes());
    }
    for label in name.split('.') {
        body.push(label.len() as u8);
        body.extend(label.as_bytes());
    }
    body.push(0);
    body.extend(1u16.to_be_bytes());
    body.extend(1u16.to_be_bytes());
    body
}

struct Written {
    path: PathBuf,
    _room: Room,
}

fn written(data: &[u8]) -> Written {
    let room = Room::new("capture");
    Written {
        path: room.file("run.pcap", data),
        _room: room,
    }
}

const EXAMPLE: [u8; 4] = [93, 184, 216, 34];

mod reading_tests {
    use super::*;

    #[test]
    fn an_outbound_connection_is_reported_with_its_destination() {
        let file = written(&little(&[ipv4(6, EXAMPLE, &tcp(443, b""))]));
        let summary = read_capture(&file.path).unwrap();
        assert_eq!(summary.packets, 1);
        assert_eq!(summary.conversations[0].protocol, "TCP");
        assert_eq!(summary.conversations[0].endpoint(), "93.184.216.34:443");
    }

    #[test]
    fn repeated_contact_is_counted_rather_than_listed_twice() {
        let frames = vec![ipv4(6, EXAMPLE, &tcp(443, b"")); 12];
        let summary = read_capture(&written(&little(&frames)).path).unwrap();
        assert_eq!(summary.conversations.len(), 1);
        assert_eq!(summary.conversations[0].packets, 12);
    }

    #[test]
    fn the_busiest_destination_is_listed_first() {
        let mut frames = vec![ipv4(6, [1, 1, 1, 1], &tcp(80, b""))];
        frames.extend(vec![ipv4(6, [2, 2, 2, 2], &tcp(443, b"")); 5]);
        let summary = read_capture(&written(&little(&frames)).path).unwrap();
        assert_eq!(summary.conversations[0].destination, "2.2.2.2");
    }

    /// Often the most legible thing in a capture: the sample says out loud
    /// where it wanted to go.
    #[test]
    fn a_dns_lookup_names_what_was_asked_for() {
        let frame = ipv4(17, [10, 0, 2, 3], &udp(53, &dns_query("evil.example.com")));
        let summary = read_capture(&written(&little(&[frame])).path).unwrap();
        assert_eq!(summary.dns_queries, ["evil.example.com"]);
    }

    #[test]
    fn a_dns_response_is_not_read_as_a_query() {
        let mut response = Vec::new();
        for field in [0x1234u16, 0x8180, 1, 1, 0, 0] {
            response.extend(field.to_be_bytes());
        }
        response.push(0);
        let frame = ipv4(17, [10, 0, 2, 3], &udp(53, &response));
        let summary = read_capture(&written(&little(&[frame])).path).unwrap();
        assert!(summary.dns_queries.is_empty());
    }

    #[test]
    fn udp_and_tcp_to_one_host_are_separate_conversations() {
        let frames = [
            ipv4(6, [1, 1, 1, 1], &tcp(443, b"")),
            ipv4(17, [1, 1, 1, 1], &udp(53, b"")),
        ];
        let summary = read_capture(&written(&little(&frames)).path).unwrap();
        let protocols: HashSet<String> = summary
            .conversations
            .iter()
            .map(|c| c.protocol.clone())
            .collect();
        assert_eq!(
            protocols,
            HashSet::from(["TCP".to_owned(), "UDP".to_owned()])
        );
        assert_eq!(summary.contacted(), ["1.1.1.1"]);
    }

    #[test]
    fn ipv6_is_read() {
        let destination: [u8; 16] = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        let mut payload = vec![0x60, 0, 0, 0, 0, 0, 6, 0x40];
        payload.extend([0u8; 16]);
        payload.extend(destination);
        payload.extend(tcp(443, b""));
        let summary = read_capture(&written(&little(&[ethernet(0x86DD, &payload)])).path).unwrap();
        assert_eq!(summary.conversations[0].destination, "2001:db8::1");
    }

    #[test]
    fn a_big_endian_capture_reads_the_same() {
        let data = pcap(&[ipv4(6, EXAMPLE, &tcp(443, b""))], true, LINKTYPE_ETHERNET);
        let summary = read_capture(&written(&data).path).unwrap();
        assert_eq!(summary.conversations[0].endpoint(), "93.184.216.34:443");
    }

    #[test]
    fn a_silent_guest_reads_as_silent() {
        let summary = read_capture(&written(&little(&[])).path).unwrap();
        assert!(summary.silent());
        assert_eq!(summary.packets, 0);
    }
}

/// These bytes came from the sandbox. None of them may panic.
mod robustness_tests {
    use super::*;

    /// QEMU creates the file the moment the machine starts.
    #[test]
    fn an_empty_file_is_a_guest_that_has_not_spoken_yet() {
        assert!(read_capture(&written(b"").path).unwrap().silent());
    }

    /// What a capture from a machine still running looks like.
    #[test]
    fn a_truncated_final_record_ends_the_read_rather_than_the_process() {
        let mut data = little(&[ipv4(6, [1, 1, 1, 1], &tcp(443, b""))]);
        for field in [0u32, 0, 9999, 9999] {
            data.extend(field.to_le_bytes());
        }
        data.extend([0u8; 4]);
        let summary = read_capture(&written(&data).path).unwrap();
        assert_eq!(summary.packets, 1);
        assert_eq!(summary.unreadable_records, 1);
    }

    #[test]
    fn a_frame_shorter_than_its_headers_claim_is_skipped() {
        let summary =
            read_capture(&written(&little(&[ethernet(0x0800, &[0x45, 0x00, 0x00])])).path).unwrap();
        assert!(summary.conversations.is_empty());
        assert_eq!(summary.unreadable_records, 1);
    }

    /// ihl claims 15 words in a frame that does not hold them.
    #[test]
    fn an_ip_header_length_that_lies_is_skipped() {
        let mut header = vec![0x4F, 0x00];
        header.extend([0u8; 18]);
        let summary = read_capture(&written(&little(&[ethernet(0x0800, &header)])).path).unwrap();
        assert!(summary.conversations.is_empty());
    }

    #[test]
    fn a_compressed_dns_name_is_declined_rather_than_guessed() {
        let mut pointer = Vec::new();
        for field in [1u16, 0x0100, 1, 0, 0, 0] {
            pointer.extend(field.to_be_bytes());
        }
        pointer.extend([0xc0, 0x0c]);
        let frame = ipv4(17, [10, 0, 2, 3], &udp(53, &pointer));
        let summary = read_capture(&written(&little(&[frame])).path).unwrap();
        assert!(summary.dns_queries.is_empty());
    }

    #[test]
    fn arp_is_noted_without_pretending_to_be_ip() {
        let summary = read_capture(&written(&little(&[ethernet(0x0806, &[0; 28])])).path).unwrap();
        assert_eq!(summary.conversations[0].protocol, "ARP");
    }

    #[test]
    fn an_unknown_ethertype_is_ignored() {
        let summary = read_capture(&written(&little(&[ethernet(0x88CC, &[0; 20])])).path).unwrap();
        assert!(summary.conversations.is_empty());
    }
}

mod refusal_tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_a_pcap_says_so() {
        let refusal =
            read_capture(&written(b"this is not a capture at all, really").path).unwrap_err();
        assert!(refusal.to_string().contains("not a pcap"));
    }

    #[test]
    fn a_missing_file_says_so() {
        assert!(read_capture(std::path::Path::new("/nowhere/run.pcap")).is_err());
    }

    /// Reading raw IP as Ethernet would produce confident nonsense.
    #[test]
    fn a_non_ethernet_capture_is_declined_rather_than_misparsed() {
        let refusal = read_capture(&written(&pcap(&[], false, 101)).path).unwrap_err();
        assert!(refusal.to_string().contains("Ethernet"));
    }
}

fn tshark_available() -> bool {
    Command::new("tshark")
        .arg("-v")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Check the format against something that is not this file. Skipped, with
/// a line saying so, where tshark is not installed.
mod agrees_with_tshark_tests {
    use super::*;

    fn tshark(file: &Written, fields: &[&str]) -> Option<String> {
        if !tshark_available() {
            eprintln!("skipped: tshark is not available");
            return None;
        }
        let mut command = Command::new("tshark");
        command.arg("-r").arg(&file.path).args(["-T", "fields"]);
        for field in fields {
            command.args(["-e", field]);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[test]
    fn tshark_reads_what_this_module_writes() {
        let file = written(&little(&[ipv4(6, EXAMPLE, &tcp(443, b""))]));
        let Some(out) = tshark(&file, &["ip.dst", "tcp.dstport"]) else {
            return;
        };
        assert!(out.contains("93.184.216.34"));
        assert!(out.contains("443"));
    }

    #[test]
    fn both_readers_find_the_same_destination() {
        let file = written(&little(&[
            ipv4(6, EXAMPLE, &tcp(443, b"")),
            ipv4(17, [10, 0, 2, 3], &udp(53, &dns_query("evil.example.com"))),
        ]));
        let Some(out) = tshark(&file, &["ip.dst"]) else {
            return;
        };
        let mine: HashSet<String> = read_capture(&file.path)
            .unwrap()
            .conversations
            .iter()
            .map(|c| c.destination.clone())
            .collect();
        let theirs: HashSet<String> = out.split_whitespace().map(str::to_owned).collect();
        assert_eq!(mine, theirs);
    }
}

/// A capture kept to a snapshot length -- BeatBox's keeps 512 bytes of each
/// packet -- still counts every byte the guest sent, not the bytes kept.
#[test]
fn a_packet_cut_short_still_counts_what_was_sent() {
    let frame = ipv4(6, [93, 184, 216, 34], &tcp(443, &[0u8; 64]));
    let mut data = little(&[]);
    data.extend(0u32.to_le_bytes());
    data.extend(0u32.to_le_bytes());
    data.extend((frame.len() as u32).to_le_bytes());
    data.extend(1500u32.to_le_bytes());
    data.extend(&frame);
    let summary = read_capture(&written(&data).path).unwrap();
    assert_eq!(summary.packets, 1);
    assert_eq!(summary.conversations[0].bytes, 1500);
}
