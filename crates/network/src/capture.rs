//! What crossed one cable, as a file Wireshark opens.
//!
//! Every frame passes one place (`switch::Net`), so a capture is a tap
//! there: on a machine's own cable, or on a cable between two devices, as
//! the frame is on that wire -- tagged on a trunk, plain on an access port.
//! It writes classic pcap (Ethernet) to a file in the network's room, and
//! stops by itself at [`MAX_BYTES`].

use std::fs::File;
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::frame;
use crate::topology::End;

/// One capture stops here.
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Past this, all of a network's captures together, the oldest go.
pub const MAX_TOTAL: u64 = 64 * 1024 * 1024;

/// Which cable.
#[derive(Debug, Clone, PartialEq)]
pub enum Tap {
    /// A machine's own cable, by its name.
    Node(String),
    /// A cable between two devices, by `switch::cable_key`.
    Link((End, End)),
}

/// What is kept.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Keep {
    /// Every frame.
    #[default]
    All,
    /// IPv4 and IPv6 only.
    Ip,
    /// Everything but ARP and discovery chatter (mDNS, SSDP, LLMNR, NetBIOS).
    Quiet,
}

impl Keep {
    /// `all`, `ip` or `quiet`.
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Ip => "ip",
            Self::Quiet => "quiet",
        }
    }

    /// The one `name` says.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "" | "all" => Some(Self::All),
            "ip" => Some(Self::Ip),
            "quiet" => Some(Self::Quiet),
            _ => None,
        }
    }
}

/// One conversation, either way: addresses, protocol and the server's port
/// (0 for any).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Talk {
    /// Who began it.
    pub source: Ipv4Addr,
    /// Who it was for.
    pub destination: Ipv4Addr,
    /// The IP protocol.
    pub protocol: u8,
    /// The destination port; 0 for any.
    pub port: u16,
}

/// One capture, running or kept.
#[derive(Debug, Clone)]
pub struct Capture {
    /// `c1`, `c2`, ...
    pub id: String,
    /// Where it listens.
    pub tap: Tap,
    /// What it keeps.
    pub keep: Keep,
    /// Only this conversation, when set.
    pub talk: Option<Talk>,
    /// The file.
    pub path: PathBuf,
    /// The file's size.
    pub bytes: u64,
    /// Frames written.
    pub frames: u64,
    /// When it began, in ms since 1970.
    pub started_ms: u64,
    /// When it stopped; `None` while it records.
    pub ended_ms: Option<u64>,
    /// Stopped by itself, at [`MAX_BYTES`].
    pub full: bool,
    /// Open while it records. Each frame is written as it comes, so a daemon
    /// that dies loses none.
    writer: Option<Arc<Mutex<File>>>,
}

/// Now, in ms since 1970.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Capture {
    /// Start writing `path`.
    pub fn start(
        id: String,
        tap: Tap,
        keep: Keep,
        talk: Option<Talk>,
        path: PathBuf,
    ) -> std::io::Result<Self> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let mut writer = File::create(&path)?;
        // Classic pcap: microseconds, Ethernet, frames up to 64 KiB.
        let mut header = Vec::with_capacity(24);
        header.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
        header.extend_from_slice(&2u16.to_le_bytes());
        header.extend_from_slice(&4u16.to_le_bytes());
        header.extend_from_slice(&[0; 8]);
        header.extend_from_slice(&65_535u32.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        writer.write_all(&header)?;
        Ok(Self {
            id,
            tap,
            keep,
            talk,
            path,
            bytes: 24,
            frames: 0,
            started_ms: now_ms(),
            ended_ms: None,
            full: false,
            writer: Some(Arc::new(Mutex::new(writer))),
        })
    }

    /// One already written, brought back from what the network wrote down.
    #[allow(clippy::too_many_arguments)]
    pub fn kept(
        id: String,
        tap: Tap,
        keep: Keep,
        path: PathBuf,
        bytes: u64,
        frames: u64,
        started_ms: u64,
        ended_ms: u64,
        full: bool,
    ) -> Self {
        Self {
            id,
            tap,
            keep,
            talk: None,
            path,
            bytes,
            frames,
            started_ms,
            ended_ms: Some(ended_ms),
            full,
            writer: None,
        }
    }

    /// Whether it still records.
    pub fn live(&self) -> bool {
        self.writer.is_some()
    }

    /// What it wrote so far, on disk.
    pub fn flush(&self) {
        if let Some(writer) = &self.writer {
            let _ = writer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .flush();
        }
    }

    /// Close the file; it stays.
    pub fn stop(&mut self) {
        if let Some(writer) = self.writer.take() {
            let _ = writer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .flush();
            self.ended_ms = Some(now_ms());
        }
    }

    /// Write `wire` if this capture keeps it; at the limit, stop.
    pub fn record(&mut self, wire: &[u8]) {
        if !self.live() || !self.wants(wire) {
            return;
        }
        let size = 16 + wire.len() as u64;
        if self.bytes + size > MAX_BYTES {
            self.full = true;
            self.stop();
            return;
        }
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let mut record = Vec::with_capacity(size as usize);
        record.extend_from_slice(&(at.as_secs() as u32).to_le_bytes());
        record.extend_from_slice(&at.subsec_micros().to_le_bytes());
        record.extend_from_slice(&(wire.len() as u32).to_le_bytes());
        record.extend_from_slice(&(wire.len() as u32).to_le_bytes());
        record.extend_from_slice(wire);
        let written = self.writer.as_ref().is_some_and(|w| {
            w.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .write_all(&record)
                .is_ok()
        });
        if written {
            self.bytes += size;
            self.frames += 1;
        } else {
            self.stop();
        }
    }

    fn wants(&self, wire: &[u8]) -> bool {
        // On a trunk the frame carries its tag; what is kept is read inside.
        let plain = frame::untag(wire).map_or_else(|| wire.to_vec(), |(_, plain)| plain);
        let ethertype = frame::ethertype(&plain);
        let packet = frame::ipv4(&plain);
        if let Some(talk) = self.talk {
            let Some(packet) = &packet else {
                return false;
            };
            let f = packet.flow;
            let there = f.source == talk.source
                && f.destination == talk.destination
                && (talk.port == 0 || f.destination_port == talk.port);
            let back = f.source == talk.destination
                && f.destination == talk.source
                && (talk.port == 0 || f.source_port == talk.port);
            if f.protocol != talk.protocol || !(there || back) {
                return false;
            }
        }
        match self.keep {
            Keep::All => true,
            Keep::Ip => matches!(ethertype, Some(frame::ETHER_IPV4 | 0x86dd)),
            Keep::Quiet => {
                ethertype != Some(frame::ETHER_ARP)
                    && !packet.is_some_and(|p| {
                        p.flow.protocol == frame::UDP
                            && matches!(
                                p.flow.destination_port,
                                5353 | 1900 | 5355 | 137 | 138 | 3702
                            )
                    })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_is_a_pcap_wireshark_reads_and_keeps_what_it_was_asked() {
        let path = std::env::temp_dir().join(format!("bb-capture-{}.pcap", std::process::id()));
        let a = Ipv4Addr::new(10, 0, 0, 1);
        let b = Ipv4Addr::new(10, 0, 0, 2);
        let mut capture = Capture::start(
            "c1".into(),
            Tap::Node("a".into()),
            Keep::Quiet,
            None,
            path.clone(),
        )
        .unwrap();
        let arp = frame::ethernet(
            frame::BROADCAST,
            [2, 0, 0, 0, 0, 1],
            frame::ETHER_ARP,
            &[0; 28],
        );
        let ping = frame::ethernet(
            [2, 0, 0, 0, 0, 2],
            [2, 0, 0, 0, 0, 1],
            frame::ETHER_IPV4,
            &frame::ipv4_packet(a, b, frame::ICMP, &frame::echo_request(1, 1)),
        );
        capture.record(&arp);
        capture.record(&ping);
        capture.stop();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(&bytes[..4], &0xa1b2_c3d4u32.to_le_bytes());
        assert_eq!(capture.frames, 1);
        assert_eq!(bytes.len() as u64, capture.bytes);
        assert_eq!(&bytes[24 + 16..], &ping[..]);
        assert!(!capture.live() && capture.ended_ms.is_some());
    }
}
