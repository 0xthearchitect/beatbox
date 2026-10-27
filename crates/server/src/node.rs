//! The Server's own node on a network's switch, for its internal services.
//!
//! The same cable a guest gets from `network.attach`, held by a thread here
//! instead of QEMU. Answers ARP and UDP for its own address on the running
//! services' ports, ignores the rest, and is policed like any node.
//!
//! **No timer.** The thread blocks on the socket and costs nothing while
//! nobody talks to it; it is stopped by an empty datagram, as the switch is.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use beatbox_engine::Data;
use serde_json::Value;

use crate::internal::Internal;

/// One internal service the node answers for.
pub struct Running {
    /// The UDP ports it answers on.
    pub ports: Vec<u16>,
    /// What answers.
    pub implementation: Box<dyn Internal>,
}

/// The internal services running, by name, shared with the node's thread.
pub type Shared = Arc<Mutex<BTreeMap<String, Running>>>;

/// `02:bb:0a:0a:0a:0a` -> the six bytes.
fn mac(text: &str) -> Option<[u8; 6]> {
    let bytes: Vec<u8> = text
        .split(':')
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect::<Option<_>>()?;
    bytes.try_into().ok()
}

fn field<'a>(answer: &'a Data, key: &str) -> std::io::Result<&'a str> {
    answer.get(key).and_then(Value::as_str).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("network.attach said no {key}"),
        )
    })
}

#[cfg(unix)]
pub use unix::Node;

#[cfg(unix)]
mod unix {
    use std::os::unix::net::UnixDatagram;
    use std::path::{Path, PathBuf};
    use std::sync::PoisonError;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use beatbox_frame as frame;

    use super::*;

    /// A node on a switch, while it runs.
    pub struct Node {
        /// Its address on that network.
        pub address: Ipv4Addr,
        port: PathBuf,
        /// The thread's socket, held here only to be shut down.
        socket: UnixDatagram,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Node {
        /// Take the cable `answer` describes -- `network.attach`'s answer --
        /// and answer for `services` on it.
        pub fn start(answer: &Data, services: Shared, name: &str) -> std::io::Result<Self> {
            let invalid =
                |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_owned());
            let address: Ipv4Addr = field(answer, "address")?
                .parse()
                .map_err(|_| invalid("network.attach said no address"))?;
            let mac =
                mac(field(answer, "mac")?).ok_or_else(|| invalid("network.attach said no mac"))?;
            let port = PathBuf::from(field(answer, "port")?);
            let switch = PathBuf::from(field(answer, "switch")?);
            // The port is this node's own (named after the Server's id): a
            // file left there is from a daemon that went away holding it.
            let _ = std::fs::remove_file(&port);
            let socket = UnixDatagram::bind(&port)?;
            let held = socket.try_clone()?;
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = Arc::clone(&stop);
            let thread = std::thread::Builder::new()
                .name(format!("beatbox-server-{name}"))
                .spawn(move || serve(&socket, &switch, address, mac, &services, &stopping))?;
            Ok(Self {
                address,
                port,
                socket: held,
                stop,
                thread: Some(thread),
            })
        }

        /// Whether the thread is still answering.
        pub fn alive(&self) -> bool {
            self.thread.as_ref().is_some_and(|t| !t.is_finished())
        }

        /// Stop answering and let go of the cable.
        pub fn stop(mut self) {
            self.halt();
        }

        /// Shutting the socket down wakes the thread's `recv` whether or not
        /// its file is still there: a datagram sent to a path somebody
        /// already removed would wake nothing, and the join would wait for
        /// ever.
        fn halt(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = self.socket.shutdown(std::net::Shutdown::Both);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = std::fs::remove_file(&self.port);
        }
    }

    impl Drop for Node {
        fn drop(&mut self) {
            if self.thread.is_some() {
                self.halt();
            }
        }
    }

    fn serve(
        socket: &UnixDatagram,
        switch: &Path,
        address: Ipv4Addr,
        mac: frame::Mac,
        services: &Mutex<BTreeMap<String, Running>>,
        stop: &AtomicBool,
    ) {
        let mut buffer = vec![0u8; 65_536];
        loop {
            let size = match socket.recv(&mut buffer) {
                Ok(_) if stop.load(Ordering::SeqCst) => return,
                Ok(size) => size,
                Err(error) => {
                    log::warn!("a server's node could not read: {error}");
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
            };
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let bytes = &buffer[..size];
            let answer = match frame::ethertype(bytes) {
                Some(frame::ETHER_ARP) => frame::arp(bytes)
                    .filter(|arp| arp.operation == 1 && arp.target_ip == address)
                    .map(|arp| frame::arp_reply(&arp, mac)),
                Some(frame::ETHER_IPV4) => {
                    let services = services.lock().unwrap_or_else(PoisonError::into_inner);
                    udp(bytes, address, mac, services)
                }
                _ => None,
            };
            if let Some(answer) = answer {
                // A switch that went away is a cable pulled out: nothing to do.
                let _ = socket.send_to(&answer, switch);
            }
        }
    }

    /// The answer to a UDP datagram for this node, from the service whose
    /// port it was sent to.
    fn udp(
        bytes: &[u8],
        address: Ipv4Addr,
        mac: frame::Mac,
        mut services: std::sync::MutexGuard<'_, BTreeMap<String, Running>>,
    ) -> Option<Vec<u8>> {
        let packet = frame::ipv4(bytes)?;
        let flow = packet.flow;
        if flow.destination != address || flow.protocol != frame::UDP {
            return None;
        }
        let port = flow.destination_port;
        let running = services.values_mut().find(|s| s.ports.contains(&port))?;
        let reply = running
            .implementation
            .udp(port, frame::udp_data(packet.payload))?;
        let datagram = frame::udp(port, flow.source_port, &reply);
        let ip = frame::ipv4_packet(address, flow.source4(), frame::UDP, &datagram);
        Some(frame::ethernet(
            frame::source(bytes),
            mac,
            frame::ETHER_IPV4,
            &ip,
        ))
    }
}

/// Where UNIX datagrams do not exist, neither does a node: the network
/// refuses there first.
#[cfg(not(unix))]
pub struct Node {
    /// Never set.
    pub address: Ipv4Addr,
}

#[cfg(not(unix))]
impl Node {
    /// Always refused.
    pub fn start(answer: &Data, _services: Shared, _name: &str) -> std::io::Result<Self> {
        let _ = (field(answer, "port"), mac(""));
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a server's node needs UNIX datagrams",
        ))
    }

    /// Never.
    pub fn alive(&self) -> bool {
        false
    }

    /// Nothing to stop.
    pub fn stop(self) {}
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use beatbox_engine::data;

    use super::*;

    #[test]
    fn it_stops_even_when_its_socket_file_is_already_gone() {
        let room = std::env::temp_dir().join(format!("bbx-node-{}", std::process::id()));
        std::fs::create_dir_all(&room).unwrap();
        let port = room.join("node.sock");
        let answer = data!({
            "address": "10.20.0.2", "mac": "02:bb:0a:14:00:02",
            "port": port.to_string_lossy(), "switch": room.join("switch.sock").to_string_lossy(),
        });
        let node = Node::start(&answer, Shared::default(), "test").unwrap();
        assert!(node.alive());
        std::fs::remove_dir_all(&room).unwrap();
        let (done, stopped) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            node.stop();
            let _ = done.send(());
        });
        assert!(
            stopped.recv_timeout(Duration::from_secs(5)).is_ok(),
            "stop hung"
        );
    }
}
