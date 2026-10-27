//! The router's way out: what guests send to the internet leaves this
//! computer on its ordinary sockets -- no privilege, no TAP, no NAT rules in
//! the kernel -- and what comes back goes to the switch as a datagram on its
//! own socket, where the firewall and the cable treat it like any frame
//! (`Net::arrive`, `Net::arrive_packet`).
//!
//! UDP is a socket per conversation. TCP cannot be carried packet by packet
//! on a socket, so the router ends the guest's connection itself, with a
//! user-space TCP stack (smoltcp, "any IP": it answers for every address
//! out there), and opens its own from this computer. A connection is only
//! accepted once the far end has been reached, so a refused or unreachable
//! address answers the guest with a reset, as a real one would.
//!
//! `Net` has already decided everything that matters -- that the router
//! leads out, that the address is outside and not this computer's own, that
//! the firewall allows it. Nothing here judges; it carries.

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as Moment;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpListenEndpoint};

use crate::frame::{self, Flow};
use crate::switch::Leaving;

/// How often the router looks at its sockets while anything is open.
/// ponytail: a poll every few ms, not epoll; ample for a lab's browsing, and
/// the first thing to change if throughput ever matters.
const LOOK: Duration = Duration::from_millis(5);
/// A UDP conversation quiet this long is forgotten; a name asked, sooner.
const QUIET: Duration = Duration::from_secs(60);
const QUIET_NAME: Duration = Duration::from_secs(10);
/// How long reaching the far end of a TCP connection may take.
const DIAL: Duration = Duration::from_secs(10);
/// Open TCP connections at once; past this a new one is reset.
/// ponytail: a fixed cap; per-machine limits if a lab ever needs fairness.
const MOST_TCP: usize = 256;
/// Each connection's buffer, each way: memory, which a sandbox spends little of.
const TCP_BUFFER: usize = 64 * 1024;

/* ── what goes back to the switch ─────────────────────────────────────── */

/// What the router hands the switch, as one datagram.
pub enum Back<'a> {
    /// A UDP answer: from where, to the guest, and the data.
    Udp(Flow, &'a [u8]),
    /// A whole IPv4 packet for a guest (TCP, from the router's stack).
    Packet(&'a [u8]),
}

fn encode_udp(back: Flow, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(13 + data.len());
    bytes.push(0);
    bytes.extend_from_slice(&back.source4().octets());
    bytes.extend_from_slice(&back.source_port.to_be_bytes());
    bytes.extend_from_slice(&back.destination4().octets());
    bytes.extend_from_slice(&back.destination_port.to_be_bytes());
    bytes.extend_from_slice(data);
    bytes
}

fn encode_packet(packet: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(1 + packet.len());
    bytes.push(1);
    bytes.extend_from_slice(packet);
    bytes
}

/// A datagram from the router, read.
pub fn decode(bytes: &[u8]) -> Option<Back<'_>> {
    match bytes.split_first()? {
        (0, rest) => {
            let head = rest.get(..12)?;
            let address =
                |at: usize| Ipv4Addr::new(head[at], head[at + 1], head[at + 2], head[at + 3]);
            let port = |at: usize| u16::from_be_bytes([head[at], head[at + 1]]);
            Some(Back::Udp(
                Flow {
                    source: address(0).into(),
                    source_port: port(4),
                    destination: address(6).into(),
                    destination_port: port(10),
                    protocol: frame::UDP,
                },
                &rest[12..],
            ))
        }
        (1, packet) => Some(Back::Packet(packet)),
        _ => None,
    }
}

/// This computer's own resolver: the first IPv4 nameserver it is set up
/// with. Names a network does not know are asked of it.
pub fn resolver() -> Option<SocketAddrV4> {
    std::fs::read_to_string("/etc/resolv.conf")
        .ok()?
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .find_map(|rest| rest.trim().parse::<Ipv4Addr>().ok())
        .map(|address| SocketAddrV4::new(address, 53))
}

/* ── the loop ─────────────────────────────────────────────────────────── */

/// Where the router is, for its TCP stack: the gateway's address and the
/// network's prefix, so every guest is on its own link.
#[derive(Debug, Clone, Copy)]
pub struct At {
    pub gateway: Ipv4Addr,
    pub prefix: u8,
}

/// Carry what leaves until the switch lets go of `leaving`.
pub fn carry_out(
    leaving: &Receiver<Leaving>,
    back: &UnixDatagram,
    switch: &Path,
    resolver: Option<SocketAddrV4>,
    at: At,
) {
    let mut udp = Udp::default();
    let mut tcp = Tcp::new(at);
    let to_switch = |bytes: Vec<u8>| {
        let _ = back.send_to(&bytes, switch);
    };
    loop {
        let first = if udp.sessions.is_empty() && tcp.idle() {
            match leaving.recv() {
                Ok(each) => Some(each),
                Err(_) => return,
            }
        } else {
            match leaving.recv_timeout(tcp.delay().min(LOOK)) {
                Ok(each) => Some(each),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        for each in first.into_iter().chain(leaving.try_iter()) {
            if each.flow.protocol == frame::TCP {
                tcp.take(each);
            } else {
                udp.send(each, resolver);
            }
        }
        tcp.step(&to_switch);
        udp.pump(&to_switch);
    }
}

/* ── UDP ──────────────────────────────────────────────────────────────── */

/// One conversation out, on a socket of this computer's.
struct Session {
    socket: UdpSocket,
    /// What its answers are: from where the guest sent, to the guest.
    back: Flow,
    last: Instant,
    quiet: Duration,
}

#[derive(Default)]
struct Udp {
    sessions: HashMap<(Flow, bool), Session>,
}

impl Udp {
    fn send(&mut self, each: Leaving, resolver: Option<SocketAddrV4>) {
        use std::collections::hash_map::Entry;
        let session = match self.sessions.entry((each.flow, each.resolve)) {
            Entry::Occupied(found) => found.into_mut(),
            Entry::Vacant(free) => {
                let target = if each.resolve {
                    let Some(resolver) = resolver else {
                        return;
                    };
                    resolver
                } else {
                    SocketAddrV4::new(each.flow.destination4(), each.flow.destination_port)
                };
                let opened = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).and_then(|socket| {
                    socket.connect(target)?;
                    socket.set_nonblocking(true)?;
                    Ok(socket)
                });
                let socket = match opened {
                    Ok(socket) => socket,
                    Err(error) => {
                        log::debug!("the router could not reach {target}: {error}");
                        return;
                    }
                };
                free.insert(Session {
                    socket,
                    back: each.flow.reverse(),
                    last: Instant::now(),
                    quiet: if each.resolve { QUIET_NAME } else { QUIET },
                })
            }
        };
        session.last = Instant::now();
        let _ = session.socket.send(each.data());
    }

    fn pump(&mut self, to_switch: &impl Fn(Vec<u8>)) {
        let mut buffer = [0u8; 65_536];
        for session in self.sessions.values_mut() {
            // Until nothing more now, or refused by the far end.
            while let Ok(size) = session.socket.recv(&mut buffer) {
                session.last = Instant::now();
                to_switch(encode_udp(session.back, &buffer[..size]));
            }
        }
        self.sessions
            .retain(|_, session| session.last.elapsed() < session.quiet);
    }
}

/* ── TCP ──────────────────────────────────────────────────────────────── */

/// The router's side of the cable, to its own TCP stack: packets from
/// guests in, packets for guests out.
#[derive(Default)]
struct Pipe {
    from_guests: VecDeque<Vec<u8>>,
    to_guests: Vec<Vec<u8>>,
}

struct Taken(Vec<u8>);
struct Giving<'a>(&'a mut Vec<Vec<u8>>);

impl phy::RxToken for Taken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Giving<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buffer = vec![0; len];
        let result = f(&mut buffer);
        self.0.push(buffer);
        result
    }
}

impl Device for Pipe {
    type RxToken<'a> = Taken;
    type TxToken<'a> = Giving<'a>;

    fn receive(&mut self, _: Moment) -> Option<(Taken, Giving<'_>)> {
        let packet = self.from_guests.pop_front()?;
        Some((Taken(packet), Giving(&mut self.to_guests)))
    }

    fn transmit(&mut self, _: Moment) -> Option<Giving<'_>> {
        Some(Giving(&mut self.to_guests))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ip;
        capabilities.max_transmission_unit = 1500;
        capabilities
    }
}

/// One guest's connection to somewhere out there.
enum Conn {
    /// Its SYN, held while the far end is being reached.
    Dialing { syn: Vec<u8> },
    /// Both ends up: bytes carried between the stack's socket and ours.
    Open {
        handle: SocketHandle,
        stream: TcpStream,
        /// From the guest, not yet taken by the far end.
        to_far: Vec<u8>,
        /// The far end has said all it will.
        far_done: bool,
        /// The guest has, and the far end was told.
        guest_done: bool,
        /// When it was accepted: a socket still waiting for its SYN long
        /// after is given up.
        since: Instant,
    },
}

struct Tcp {
    iface: Interface,
    pipe: Pipe,
    sockets: SocketSet<'static>,
    /// By the guest's own flow: its address and port, and where it asked to go.
    conns: HashMap<Flow, Conn>,
    dial: Sender<(Flow, std::io::Result<TcpStream>)>,
    dialed: Receiver<(Flow, std::io::Result<TcpStream>)>,
}

impl Tcp {
    fn new(at: At) -> Self {
        let mut pipe = Pipe::default();
        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        let mut iface = Interface::new(config, &mut pipe, Moment::now());
        iface.update_ip_addrs(|addresses| {
            let _ = addresses.push(IpCidr::new(IpAddress::Ipv4(at.gateway), at.prefix));
        });
        // Every address out there is the router's to answer for.
        iface.set_any_ip(true);
        let (dial, dialed) = mpsc::channel();
        Self {
            iface,
            pipe,
            sockets: SocketSet::new(Vec::new()),
            conns: HashMap::new(),
            dial,
            dialed,
        }
    }

    fn idle(&self) -> bool {
        self.conns.is_empty() && self.pipe.from_guests.is_empty()
    }

    /// How long until the stack has something to do on its own: a
    /// retransmission, a timeout.
    fn delay(&mut self) -> Duration {
        self.iface
            .poll_delay(Moment::now(), &self.sockets)
            .map_or(LOOK, |d| Duration::from_micros(d.total_micros()))
    }

    fn take(&mut self, each: Leaving) {
        let flags = each.tcp_flags();
        let opening = flags & 0x02 != 0 && flags & 0x10 == 0;
        let room = self.conns.len() < MOST_TCP;
        match self.conns.get_mut(&each.flow) {
            // A SYN sent again while the far end is still being reached.
            Some(Conn::Dialing { syn }) => {
                if opening {
                    *syn = each.packet;
                }
            }
            Some(Conn::Open { .. }) => self.pipe.from_guests.push_back(each.packet),
            None if opening && room => {
                let flow = each.flow;
                self.conns.insert(flow, Conn::Dialing { syn: each.packet });
                let dial = self.dial.clone();
                let far = SocketAddr::V4(SocketAddrV4::new(
                    flow.destination4(),
                    flow.destination_port,
                ));
                let spawned = std::thread::Builder::new()
                    .name("beatbox-router-dial".into())
                    .spawn(move || {
                        let _ = dial.send((flow, TcpStream::connect_timeout(&far, DIAL)));
                    });
                if spawned.is_err() {
                    self.conns.remove(&flow);
                }
            }
            // Anything else -- too many open, or the tail of a connection
            // already forgotten -- goes to the stack, which has no socket for
            // it and answers with a reset.
            None => self.pipe.from_guests.push_back(each.packet),
        }
    }

    fn step(&mut self, to_switch: &impl Fn(Vec<u8>)) {
        while let Ok((flow, reached)) = self.dialed.try_recv() {
            self.reached(flow, reached);
        }
        let now = Moment::now();
        self.iface.poll(now, &mut self.pipe, &mut self.sockets);
        self.pump();
        self.iface.poll(now, &mut self.pipe, &mut self.sockets);
        for packet in self.pipe.to_guests.drain(..) {
            to_switch(encode_packet(&packet));
        }
    }

    /// The far end answered, or did not. Either way the guest's SYN goes to
    /// the stack: with a socket listening for it, it is accepted; without,
    /// it is reset.
    fn reached(&mut self, flow: Flow, reached: std::io::Result<TcpStream>) {
        let Some(Conn::Dialing { syn }) = self.conns.remove(&flow) else {
            return;
        };
        let opened = reached.and_then(|stream| {
            stream.set_nonblocking(true)?;
            stream.set_nodelay(true)?;
            Ok(stream)
        });
        match opened {
            Ok(stream) => {
                let mut socket = tcp::Socket::new(
                    tcp::SocketBuffer::new(vec![0; TCP_BUFFER]),
                    tcp::SocketBuffer::new(vec![0; TCP_BUFFER]),
                );
                socket.set_timeout(Some(smoltcp::time::Duration::from_secs(60)));
                let listening = socket.listen(IpListenEndpoint {
                    addr: Some(IpAddress::Ipv4(flow.destination4())),
                    port: flow.destination_port,
                });
                if listening.is_ok() {
                    let handle = self.sockets.add(socket);
                    self.conns.insert(
                        flow,
                        Conn::Open {
                            handle,
                            stream,
                            to_far: Vec::new(),
                            far_done: false,
                            guest_done: false,
                            since: Instant::now(),
                        },
                    );
                }
            }
            Err(error) => {
                log::debug!(
                    "the router could not reach {}:{}: {error}",
                    flow.destination,
                    flow.destination_port
                );
            }
        }
        self.pipe.from_guests.push_back(syn);
    }

    /// Bytes across, each way, as far as both ends take them now.
    fn pump(&mut self) {
        let mut buffer = [0u8; 16 * 1024];
        let mut gone = Vec::new();
        for (flow, conn) in &mut self.conns {
            let Conn::Open {
                handle,
                stream,
                to_far,
                far_done,
                guest_done,
                since,
            } = conn
            else {
                continue;
            };
            let socket = self.sockets.get_mut::<tcp::Socket>(*handle);
            let mut broken = false;

            // Guest to far end.
            while to_far.len() < TCP_BUFFER && socket.can_recv() {
                match socket.recv_slice(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => to_far.extend_from_slice(&buffer[..size]),
                }
            }
            while !to_far.is_empty() {
                match stream.write(to_far) {
                    Ok(0) => {
                        broken = true;
                        break;
                    }
                    Ok(size) => {
                        to_far.drain(..size);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => {
                        broken = true;
                        break;
                    }
                }
            }
            // The guest sent its FIN: everything it said has gone across.
            let guest_finished = matches!(
                socket.state(),
                tcp::State::CloseWait
                    | tcp::State::LastAck
                    | tcp::State::Closing
                    | tcp::State::TimeWait
            );
            if !*guest_done && to_far.is_empty() && guest_finished {
                let _ = stream.shutdown(Shutdown::Write);
                *guest_done = true;
            }

            // Far end to guest.
            while !*far_done && socket.can_send() {
                let room = socket.send_capacity() - socket.send_queue();
                if room == 0 {
                    break;
                }
                let take = room.min(buffer.len());
                match stream.read(&mut buffer[..take]) {
                    Ok(0) => {
                        *far_done = true;
                        socket.close();
                    }
                    Ok(size) => {
                        let _ = socket.send_slice(&buffer[..size]);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => {
                        broken = true;
                        break;
                    }
                }
            }

            if broken {
                socket.abort();
            }
            let never_came = socket.state() == tcp::State::Listen && since.elapsed() > DIAL;
            if never_came || matches!(socket.state(), tcp::State::Closed | tcp::State::TimeWait) {
                gone.push(*flow);
            }
        }
        for flow in gone {
            if let Some(Conn::Open { handle, .. }) = self.conns.remove(&flow) {
                self.sockets.remove(handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_goes_back_reads_as_it_was_written() {
        let flow = Flow {
            source: Ipv4Addr::new(1, 1, 1, 1).into(),
            destination: Ipv4Addr::new(10, 10, 10, 10).into(),
            protocol: frame::UDP,
            source_port: 53,
            destination_port: 40000,
        };
        match decode(&encode_udp(flow, b"answer")) {
            Some(Back::Udp(got, data)) => {
                assert_eq!(got, flow);
                assert_eq!(data, b"answer");
            }
            _ => panic!("not a UDP answer"),
        }
        match decode(&encode_packet(b"\x45ip")) {
            Some(Back::Packet(packet)) => assert_eq!(packet, b"\x45ip"),
            _ => panic!("not a packet"),
        }
        assert!(decode(&[7, 1, 2]).is_none());
    }

    const AT: At = At {
        gateway: Ipv4Addr::new(10, 10, 10, 1),
        prefix: 24,
    };
    const GUEST: Ipv4Addr = Ipv4Addr::new(10, 10, 10, 10);

    /// The router running on its own thread, with a socket standing in for
    /// the switch it answers to.
    struct Running {
        leave: Option<Sender<Leaving>>,
        switch: UnixDatagram,
        thread: Option<std::thread::JoinHandle<()>>,
        room: std::path::PathBuf,
    }

    impl Running {
        fn start(name: &str) -> Self {
            let room =
                std::env::temp_dir().join(format!("bb-router-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&room);
            std::fs::create_dir_all(&room).unwrap();
            let switch_path = room.join("s.sock");
            let switch = UnixDatagram::bind(&switch_path).unwrap();
            switch
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            let back = UnixDatagram::bind(room.join("u.sock")).unwrap();
            let (leave, leaving) = mpsc::channel();
            let thread =
                std::thread::spawn(move || carry_out(&leaving, &back, &switch_path, None, AT));
            Self {
                leave: Some(leave),
                switch,
                thread: Some(thread),
                room,
            }
        }

        fn send(&self, packet: Vec<u8>) {
            let probe = frame::ethernet(
                frame::GATEWAY_MAC,
                frame::GATEWAY_MAC,
                frame::ETHER_IPV4,
                &packet,
            );
            let flow = frame::ipv4(&probe).unwrap().flow;
            self.leave
                .as_ref()
                .unwrap()
                .send(Leaving {
                    flow,
                    packet,
                    resolve: false,
                })
                .unwrap();
        }

        fn heard(&self) -> Option<Vec<u8>> {
            let mut buffer = vec![0u8; 70_000];
            let size = self.switch.recv(&mut buffer).ok()?;
            Some(buffer[..size].to_vec())
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            drop(self.leave.take());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = std::fs::remove_dir_all(&self.room);
        }
    }

    #[test]
    fn udp_goes_out_and_the_answer_comes_back_to_the_switch() {
        let router = Running::start("udp");
        // Out there: an echo on this computer. The router itself does not
        // judge where it may go -- `Net` did, before it was ever sent.
        let far = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = far.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 64];
            if let Ok((size, from)) = far.recv_from(&mut buffer) {
                let _ = far.send_to(&buffer[..size], from);
            }
        });
        let datagram = frame::udp(40000, port, b"echo");
        router.send(frame::ipv4_packet(
            GUEST,
            Ipv4Addr::LOCALHOST,
            frame::UDP,
            &datagram,
        ));
        let heard = (0..250).find_map(|_| router.heard()).expect("an answer");
        match decode(&heard) {
            Some(Back::Udp(flow, data)) => {
                assert_eq!(flow.source, Ipv4Addr::LOCALHOST);
                assert_eq!(flow.source_port, port);
                assert_eq!(flow.destination, GUEST);
                assert_eq!(flow.destination_port, 40000);
                assert_eq!(data, b"echo");
            }
            _ => panic!("not a UDP answer"),
        }
    }

    /// A guest with a TCP stack of its own -- smoltcp again, on the other
    /// side of the cable -- connecting through the router to `far`.
    struct Guest {
        iface: Interface,
        pipe: Pipe,
        sockets: SocketSet<'static>,
        handle: SocketHandle,
    }

    impl Guest {
        fn connect(far: SocketAddrV4) -> Self {
            Self::connect_as(far, GUEST, AT.gateway)
        }

        /// From `me`, whose way out is `gateway`.
        fn connect_as(far: SocketAddrV4, me: Ipv4Addr, gateway: Ipv4Addr) -> Self {
            let mut pipe = Pipe::default();
            let mut iface =
                Interface::new(Config::new(HardwareAddress::Ip), &mut pipe, Moment::now());
            iface.update_ip_addrs(|addresses| {
                let _ = addresses.push(IpCidr::new(IpAddress::Ipv4(me), 24));
            });
            iface.routes_mut().add_default_ipv4_route(gateway).unwrap();
            let mut socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 4096]),
                tcp::SocketBuffer::new(vec![0; 4096]),
            );
            socket
                .connect(
                    iface.context(),
                    (IpAddress::Ipv4(*far.ip()), far.port()),
                    49152,
                )
                .unwrap();
            let mut sockets = SocketSet::new(Vec::new());
            let handle = sockets.add(socket);
            Self {
                iface,
                pipe,
                sockets,
                handle,
            }
        }

        /// One turn: the guest's stack, its packets to the router, the
        /// router's answers back in.
        fn turn(&mut self, router: &Running) {
            self.iface
                .poll(Moment::now(), &mut self.pipe, &mut self.sockets);
            for packet in self.pipe.to_guests.drain(..) {
                router.send(packet);
            }
            while let Some(heard) = router.heard() {
                if let Some(Back::Packet(ip)) = decode(&heard) {
                    self.pipe.from_guests.push_back(ip.to_vec());
                }
            }
        }

        fn socket(&mut self) -> &mut tcp::Socket<'static> {
            self.sockets.get_mut::<tcp::Socket>(self.handle)
        }
    }

    #[test]
    fn a_guest_talks_tcp_to_out_there_through_the_router_and_hangs_up() {
        let router = Running::start("tcp");
        let far = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(far_address) = far.local_addr().unwrap() else {
            unreachable!()
        };
        let served = std::thread::spawn(move || {
            let (mut stream, _) = far.accept().unwrap();
            let mut heard = Vec::new();
            stream.read_to_end(&mut heard).unwrap();
            stream.write_all(&heard).unwrap();
            stream.write_all(b" -- bye").unwrap();
            heard
        });
        let mut guest = Guest::connect(far_address);
        let mut sent = false;
        let mut answer = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            guest.turn(&router);
            let socket = guest.socket();
            if socket.can_send() && !sent {
                socket.send_slice(b"hello from the lab").unwrap();
                // Said all it will: the far end reads to its end, answers, closes.
                socket.close();
                sent = true;
            }
            while socket.can_recv() {
                let mut buffer = [0u8; 256];
                let size = socket.recv_slice(&mut buffer).unwrap();
                answer.extend_from_slice(&buffer[..size]);
            }
            if sent && !socket.is_active() {
                break;
            }
        }
        assert_eq!(served.join().unwrap(), b"hello from the lab");
        assert_eq!(answer, b"hello from the lab -- bye");
        assert!(!guest.socket().is_active(), "both sides closed");
    }

    #[test]
    fn a_guest_in_another_of_the_routers_networks_reaches_out_there_too() {
        let router = Running::start("vlan");
        let far = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(far_address) = far.local_addr().unwrap() else {
            unreachable!()
        };
        let served = std::thread::spawn(move || {
            let (mut stream, _) = far.accept().unwrap();
            stream.write_all(b"hi").unwrap();
        });
        // A VLAN's network, not the main one (`Net::interfaces`).
        let mut guest = Guest::connect_as(
            far_address,
            Ipv4Addr::new(10, 20, 0, 10),
            Ipv4Addr::new(10, 20, 0, 1),
        );
        let mut answer = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && answer.len() < 2 {
            guest.turn(&router);
            let socket = guest.socket();
            while socket.can_recv() {
                let mut buffer = [0u8; 16];
                let size = socket.recv_slice(&mut buffer).unwrap();
                answer.extend_from_slice(&buffer[..size]);
            }
        }
        served.join().unwrap();
        assert_eq!(answer, b"hi");
    }

    #[test]
    fn a_far_end_that_refuses_answers_the_guest_with_a_reset() {
        let router = Running::start("refused");
        // A port nothing listens on: bound, then let go.
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(far_address) = gone.local_addr().unwrap() else {
            unreachable!()
        };
        drop(gone);
        let mut guest = Guest::connect(far_address);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && guest.socket().state() != tcp::State::Closed {
            guest.turn(&router);
        }
        assert_eq!(
            guest.socket().state(),
            tcp::State::Closed,
            "reset, not left waiting"
        );
    }
}
