//! The cable itself: one datagram per frame, between two ends.
//!
//! An end is a UNIX datagram socket's path, or a UDP port on 127.0.0.1
//! where UNIX datagrams do not exist (Windows). QEMU's `-netdev dgram` takes
//! either, so the switch, the light PCs and a Server's nodes only change
//! which socket they hold, never what they do with a frame.

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::time::Duration;

/// How a UDP end is written: `udp:127.0.0.1:<port>`.
const UDP_PREFIX: &str = "udp:127.0.0.1:";

/// Where one end of a cable listens.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Port {
    /// A UNIX datagram socket.
    Path(PathBuf),
    /// A UDP port on 127.0.0.1, never any other address: a cable never
    /// leaves this computer.
    Udp(u16),
}

impl Port {
    /// Read back what [`Port`]'s `Display` wrote: `udp:127.0.0.1:<port>`,
    /// or else a path.
    pub fn parse(text: &str) -> Self {
        text.strip_prefix(UDP_PREFIX)
            .and_then(|port| port.parse().ok())
            .map_or_else(|| Self::Path(PathBuf::from(text)), Self::Udp)
    }

    /// Take away what a socket left: a path's file. A UDP port leaves
    /// nothing behind.
    pub fn forget(&self) {
        if let Self::Path(path) = self {
            let _ = std::fs::remove_file(path);
        }
    }

    fn udp(port: u16) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
    }
}

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Udp(port) => write!(f, "{UDP_PREFIX}{port}"),
        }
    }
}

impl From<PathBuf> for Port {
    fn from(path: PathBuf) -> Self {
        Self::Path(path)
    }
}

/// A socket on one end of a cable.
#[derive(Debug)]
pub enum Socket {
    /// See [`Port::Path`].
    #[cfg(unix)]
    Unix(UnixDatagram),
    /// See [`Port::Udp`].
    Udp(UdpSocket),
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "UNIX datagram sockets do not exist on this system",
    )
}

fn other_kind() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "a UNIX socket and a UDP one cannot talk",
    )
}

impl Socket {
    /// Listen at `port`. A path's file left there by a daemon that went
    /// away is taken over.
    pub fn bind(port: &Port) -> io::Result<Self> {
        match port {
            #[cfg(unix)]
            Port::Path(path) => {
                let _ = std::fs::remove_file(path);
                UnixDatagram::bind(path).map(Self::Unix)
            }
            #[cfg(not(unix))]
            Port::Path(_) => Err(unsupported()),
            Port::Udp(number) => UdpSocket::bind(Port::udp(*number)).map(Self::Udp),
        }
    }

    /// A socket that only sends towards ports like `like`. Unnamed for a
    /// path; for UDP, a port the system picks.
    pub fn sender(like: &Port) -> io::Result<Self> {
        match like {
            #[cfg(unix)]
            Port::Path(_) => UnixDatagram::unbound().map(Self::Unix),
            #[cfg(not(unix))]
            Port::Path(_) => Err(unsupported()),
            Port::Udp(_) => UdpSocket::bind(Port::udp(0)).map(Self::Udp),
        }
    }

    /// Where this socket listens, if it was bound to anywhere.
    pub fn port(&self) -> io::Result<Port> {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => socket
                .local_addr()?
                .as_pathname()
                .map(|path| Port::Path(path.to_owned()))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "an unnamed socket")),
            Self::Udp(socket) => Ok(Port::Udp(socket.local_addr()?.port())),
        }
    }

    /// One frame to `to`.
    pub fn send_to(&self, bytes: &[u8], to: &Port) -> io::Result<usize> {
        match (self, to) {
            #[cfg(unix)]
            (Self::Unix(socket), Port::Path(path)) => socket.send_to(bytes, path),
            (Self::Udp(socket), Port::Udp(port)) => socket.send_to(bytes, Port::udp(*port)),
            _ => Err(other_kind()),
        }
    }

    /// One frame in.
    pub fn recv(&self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => socket.recv(buffer),
            Self::Udp(socket) => socket.recv(buffer),
        }
    }

    /// One frame in, and which port it came from. `None` for a sender with
    /// no name, or one that is not on this computer: never a cable.
    pub fn recv_from(&self, buffer: &mut [u8]) -> io::Result<(usize, Option<Port>)> {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => {
                let (size, from) = socket.recv_from(buffer)?;
                Ok((size, from.as_pathname().map(|p| Port::Path(p.to_owned()))))
            }
            Self::Udp(socket) => {
                let (size, from) = socket.recv_from(buffer)?;
                let from = match from {
                    SocketAddr::V4(from) if from.ip().is_loopback() => Some(Port::Udp(from.port())),
                    _ => None,
                };
                Ok((size, from))
            }
        }
    }

    /// How long a read waits; `None` for ever.
    pub fn set_read_timeout(&self, wait: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => socket.set_read_timeout(wait),
            Self::Udp(socket) => socket.set_read_timeout(wait),
        }
    }

    /// Whether reads and sends return at once instead of waiting.
    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => socket.set_nonblocking(on),
            Self::Udp(socket) => socket.set_nonblocking(on),
        }
    }

    /// The same socket, a second handle.
    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => socket.try_clone().map(Self::Unix),
            Self::Udp(socket) => socket.try_clone().map(Self::Udp),
        }
    }

    /// Wake whoever waits in a read on this socket, through another handle
    /// of it. A UNIX socket is shut down: a datagram to a path somebody
    /// already removed would wake nothing. UDP cannot be shut down, so it
    /// gets an empty datagram from itself, which no cable sends.
    pub fn wake(&self) {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
            Self::Udp(socket) => {
                if let Ok(at) = socket.local_addr() {
                    let _ = socket.send_to(&[], at);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_reads_back_what_it_wrote() {
        let udp = Port::Udp(47_000);
        assert_eq!(udp.to_string(), "udp:127.0.0.1:47000");
        assert_eq!(Port::parse(&udp.to_string()), udp);
        let path = Port::Path(PathBuf::from("/run/x/switch.sock"));
        assert_eq!(Port::parse(&path.to_string()), path);
        // Only this computer, and only a number: anything else is a path.
        assert!(matches!(Port::parse("udp:10.0.0.1:47000"), Port::Path(_)));
        assert!(matches!(Port::parse("udp:127.0.0.1:x"), Port::Path(_)));
    }

    #[test]
    fn a_frame_crosses_a_udp_cable_and_says_where_it_came_from() {
        let switch = Socket::bind(&Port::Udp(0)).unwrap();
        let switch_port = switch.port().unwrap();
        let guest = Socket::bind(&Port::Udp(0)).unwrap();
        let guest_port = guest.port().unwrap();
        guest.send_to(b"frame", &switch_port).unwrap();
        let mut buffer = [0u8; 16];
        switch
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (size, from) = switch.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..size], b"frame");
        assert_eq!(from, Some(guest_port));
    }

    #[test]
    fn a_udp_reader_is_woken_from_another_handle() {
        let socket = Socket::bind(&Port::Udp(0)).unwrap();
        let held = socket.try_clone().unwrap();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0u8; 16];
            socket.recv(&mut buffer)
        });
        held.wake();
        assert_eq!(reader.join().unwrap().unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_frame_crosses_a_unix_cable() {
        let room = std::env::temp_dir().join(format!("bbx-link-{}", std::process::id()));
        std::fs::create_dir_all(&room).unwrap();
        let switch_port = Port::Path(room.join("s.sock"));
        let switch = Socket::bind(&switch_port).unwrap();
        let guest_port = Port::Path(room.join("g.sock"));
        let guest = Socket::bind(&guest_port).unwrap();
        guest.send_to(b"frame", &switch_port).unwrap();
        let mut buffer = [0u8; 16];
        let (size, from) = switch.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..size], b"frame");
        assert_eq!(from, Some(guest_port));
        assert!(
            Socket::sender(&switch_port)
                .unwrap()
                .send_to(b"x", &Port::Udp(1))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(room);
    }
}
