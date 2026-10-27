//! Light machines: a PC or a small server with nothing but a network,
//! inside BeatBox itself -- the Packet Tracer kind, for testing a network
//! without a virtual machine's gigabytes.
//!
//! Each is one thread holding a user-space stack (smoltcp) on the machine's
//! own cable: it binds the node's socket and speaks frames to the switch the
//! way QEMU does, so the switch, the firewall and the routers cannot tell it
//! from a guest. It asks DHCP for its address (or keeps one it was given),
//! answers ping, and, when told to, serves a small web page and UDP echo.
//! Its terminal runs `ipconfig`, `ping`, `traceroute`, `nslookup`, `curl`
//! and `nc` on that stack; each answer is a line with a code the page puts
//! in words.

use std::net::Ipv4Addr;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::frame::Mac;

/// What a light machine is and answers. Its address is the node's: the
/// network gives it, by DHCP or by being told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightConf {
    /// Drawn and named as a server rather than a PC.
    pub server: bool,
    /// Answers ping.
    pub ping: bool,
    /// Serves a small page on port 80.
    pub web: bool,
    /// Echoes UDP on port 7.
    pub echo: bool,
    /// Asks DHCP; otherwise keeps the node's address and the gateway and
    /// DNS below.
    pub dhcp: bool,
    /// When not by DHCP: the way out; the network's router when none.
    pub gateway: Option<Ipv4Addr>,
    /// When not by DHCP: who answers names; the gateway when none.
    pub dns: Option<Ipv4Addr>,
}

impl LightConf {
    /// A PC: answers ping, asks DHCP.
    pub fn pc() -> Self {
        Self {
            server: false,
            ping: true,
            web: false,
            echo: false,
            dhcp: true,
            gateway: None,
            dns: None,
        }
    }

    /// A server: a page on port 80 and echo on 7 as well.
    pub fn server() -> Self {
        Self {
            server: true,
            web: true,
            echo: true,
            ..Self::pc()
        }
    }

    /// As `net.json` and the page see it.
    pub fn to_value(&self) -> Value {
        json!({
            "kind": if self.server { "server" } else { "pc" },
            "ping": self.ping, "web": self.web, "echo": self.echo, "dhcp": self.dhcp,
            "gateway": self.gateway.map(|a| a.to_string()),
            "dns": self.dns.map(|a| a.to_string()),
        })
    }

    /// Read back what [`LightConf::to_value`] wrote; missing parts are a PC's.
    pub fn from_value(value: &Value) -> Self {
        let base = if value["kind"] == "server" {
            Self::server()
        } else {
            Self::pc()
        };
        let flag = |key: &str, default: bool| value[key].as_bool().unwrap_or(default);
        let address = |key: &str| value[key].as_str().and_then(|a| a.parse().ok());
        Self {
            ping: flag("ping", base.ping),
            web: flag("web", base.web),
            echo: flag("echo", base.echo),
            dhcp: flag("dhcp", base.dhcp),
            gateway: address("gateway"),
            dns: address("dns"),
            ..base
        }
    }
}

/// Everything a light machine starts from. When any of it changes the
/// machine is started again (`NetworkSandbox::sync_lights`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    /// Its node's name.
    pub name: String,
    /// Its node's MAC.
    pub mac: Mac,
    /// Its end of the cable.
    pub port: PathBuf,
    /// The switch's socket.
    pub switch: PathBuf,
    /// Its address and prefix, as the network gives them.
    pub address: Ipv4Addr,
    /// Its prefix length.
    pub prefix: u8,
    /// The way out, when not by DHCP.
    pub gateway: Ipv4Addr,
    /// Who answers names, when not by DHCP.
    pub dns: Ipv4Addr,
    /// What it is.
    pub conf: LightConf,
}

/// The running part: a thread and a socket, which only Unix has here.
#[cfg(unix)]
mod machine {
    use std::io::ErrorKind;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::os::unix::net::UnixDatagram;
    use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
    use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
    use smoltcp::socket::{dhcpv4, raw, tcp, udp};
    use smoltcp::time::Instant as Moment;
    use smoltcp::wire::{
        EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpProtocol, IpVersion,
        Ipv4Cidr,
    };

    use super::Start;
    use crate::frame;

    type Ask = (String, Sender<Vec<Value>>);

    /// A running light machine.
    pub struct Light {
        /// What it was started from.
        pub start: Start,
        ask: Option<Sender<Ask>>,
        thread: Option<JoinHandle<()>>,
    }

    impl Light {
        /// Start it on its cable.
        pub fn spawn(start: Start) -> std::io::Result<Self> {
            let _ = std::fs::remove_file(&start.port);
            let socket = UnixDatagram::bind(&start.port)?;
            let (ask, asks) = mpsc::channel();
            let first = start.clone();
            let thread = std::thread::Builder::new()
                .name(format!("beatbox-light-{}", start.name))
                .stack_size(256 * 1024)
                .spawn(move || live(first, socket, &asks))?;
            Ok(Self {
                start,
                ask: Some(ask),
                thread: Some(thread),
            })
        }

        /// Run one terminal line; its answer, line by line. Within `timeout`.
        pub fn run(&self, line: &str, timeout: Duration) -> Vec<Value> {
            let Some(answer) = self.ask(line) else {
                return vec![json!({"code": "off"})];
            };
            answer
                .recv_timeout(timeout)
                .unwrap_or_else(|_| vec![json!({"code": "timeout"})])
        }

        /// Hand one terminal line over and come back later for its answer:
        /// `None` when the machine is not running.
        pub fn ask(&self, line: &str) -> Option<mpsc::Receiver<Vec<Value>>> {
            let (reply, answer) = mpsc::channel();
            let ask = self.ask.as_ref()?;
            ask.send((line.to_owned(), reply)).ok()?;
            Some(answer)
        }

        /// Stop it, and take its socket away.
        pub fn stop(mut self) {
            self.halt();
        }

        fn halt(&mut self) {
            drop(self.ask.take());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = std::fs::remove_file(&self.start.port);
        }
    }

    impl Drop for Light {
        fn drop(&mut self) {
            if self.thread.is_some() {
                self.halt();
            }
        }
    }

    /* ── the machine's thread ─────────────────────────────────────────────── */

    fn live(start: Start, socket: UnixDatagram, asks: &Receiver<Ask>) {
        let mut machine = Machine::new(start, socket);
        loop {
            match asks.try_recv() {
                Ok((line, reply)) => {
                    let said = machine.command(&line);
                    let _ = reply.send(said);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return,
            }
            machine.pump(Duration::from_millis(40));
        }
    }

    /// Frames from the cable in, frames for the cable out.
    #[derive(Default)]
    struct Wire {
        from_cable: std::collections::VecDeque<Vec<u8>>,
        to_cable: Vec<Vec<u8>>,
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

    impl Device for Wire {
        type RxToken<'a> = Taken;
        type TxToken<'a> = Giving<'a>;

        fn receive(&mut self, _: Moment) -> Option<(Taken, Giving<'_>)> {
            let frame = self.from_cable.pop_front()?;
            Some((Taken(frame), Giving(&mut self.to_cable)))
        }

        fn transmit(&mut self, _: Moment) -> Option<Giving<'_>> {
            Some(Giving(&mut self.to_cable))
        }

        fn capabilities(&self) -> DeviceCapabilities {
            let mut capabilities = DeviceCapabilities::default();
            capabilities.medium = Medium::Ethernet;
            capabilities.max_transmission_unit = 1514;
            capabilities
        }
    }

    /// The ICMP identifier this machine's pings carry.
    const IDENT: u16 = 0x4242;

    struct Machine {
        start: Start,
        socket: UnixDatagram,
        wire: Wire,
        iface: Interface,
        sockets: SocketSet<'static>,
        raw: SocketHandle,
        /// ICMPv6: its pings, and what routers say about them.
        raw6: SocketHandle,
        names: SocketHandle,
        dhcp: Option<SocketHandle>,
        echo: Option<SocketHandle>,
        web: Option<SocketHandle>,
        /// A server also takes SSH connections, to be counted, not used.
        ssh: Option<SocketHandle>,
        /// Where it is now: by DHCP, or as it was told.
        address: Option<Ipv4Cidr>,
        gateway: Option<Ipv4Addr>,
        dns: Option<Ipv4Addr>,
        sequence: u16,
        port: u16,
        /// ICMP that is not an echo request, kept for the probe waiting on it.
        heard: std::collections::VecDeque<Vec<u8>>,
    }

    impl Machine {
        fn new(start: Start, socket: UnixDatagram) -> Self {
            let mut wire = Wire::default();
            let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(start.mac)));
            // Its IPv6 address made from the router's advertisement.
            config.slaac = true;
            config.random_seed = u64::from_be_bytes([
                0,
                0,
                start.mac[0],
                start.mac[1],
                start.mac[2],
                start.mac[3],
                start.mac[4],
                start.mac[5],
            ]) | 1;
            let mut iface = Interface::new(config, &mut wire, Moment::now());
            iface.update_ip_addrs(|addresses| {
                let _ = addresses.push(IpCidr::new(
                    IpAddress::Ipv6(frame::link_local(start.mac)),
                    64,
                ));
            });
            let mut sockets = SocketSet::new(Vec::new());
            let raw = sockets.add(raw::Socket::new(
                Some(IpVersion::Ipv4),
                Some(IpProtocol::Icmp),
                raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 16], vec![0; 16 * 1600]),
                raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 8], vec![0; 8 * 1600]),
            ));
            let raw6 = sockets.add(raw::Socket::new(
                Some(IpVersion::Ipv6),
                Some(IpProtocol::Icmpv6),
                raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 16], vec![0; 16 * 1600]),
                raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 8], vec![0; 8 * 1600]),
            ));
            let names = sockets.add(udp_socket());
            let dhcp = start.conf.dhcp.then(|| sockets.add(dhcpv4::Socket::new()));
            let echo = start.conf.echo.then(|| {
                let mut socket = udp_socket();
                let _ = socket.bind(7);
                sockets.add(socket)
            });
            // A server's one connection is asked now and then whether the
            // other end is still there: one a firewall cut half-way would
            // otherwise hold the port.
            let served = || {
                let mut socket = tcp_socket();
                socket.set_keep_alive(Some(smoltcp::time::Duration::from_secs(1)));
                socket
            };
            let web = start.conf.web.then(|| sockets.add(served()));
            let ssh = start.conf.web.then(|| sockets.add(served()));
            let mut machine = Self {
                start,
                socket,
                wire,
                iface,
                sockets,
                raw,
                raw6,
                names,
                dhcp,
                echo,
                web,
                ssh,
                address: None,
                gateway: None,
                dns: None,
                sequence: 0,
                port: 49152,
                heard: std::collections::VecDeque::new(),
            };
            if !machine.start.conf.dhcp {
                let (address, prefix) = (machine.start.address, machine.start.prefix);
                let gateway = machine.start.conf.gateway.unwrap_or(machine.start.gateway);
                let dns = machine.start.conf.dns.unwrap_or(machine.start.dns);
                machine.settle(
                    Some(Ipv4Cidr::new(address, prefix)),
                    Some(gateway),
                    Some(dns),
                );
            }
            let names = machine.names;
            let _ = machine.sockets.get_mut::<udp::Socket>(names).bind(5353);
            machine
        }

        fn settle(
            &mut self,
            address: Option<Ipv4Cidr>,
            gateway: Option<Ipv4Addr>,
            dns: Option<Ipv4Addr>,
        ) {
            self.iface.update_ip_addrs(|addresses| {
                // IPv6 is the router's advertisement's business, not DHCP's.
                addresses.retain(|a| matches!(a, IpCidr::Ipv6(_)));
                if let Some(address) = address {
                    let _ = addresses.push(IpCidr::Ipv4(address));
                }
            });
            self.iface.routes_mut().remove_default_ipv4_route();
            if let Some(gateway) = gateway {
                let _ = self.iface.routes_mut().add_default_ipv4_route(gateway);
            }
            self.address = address;
            self.gateway = gateway;
            self.dns = dns;
        }

        /// Frames in for up to `wait`, the stack, its services, frames out.
        fn pump(&mut self, wait: Duration) {
            // What is queued goes first, so nothing waits behind the wait.
            self.cycle();
            let _ = self
                .socket
                .set_read_timeout(Some(wait.max(Duration::from_millis(1))));
            let mut buffer = [0u8; 2048];
            if let Ok(size) = self.socket.recv(&mut buffer) {
                self.take(&buffer[..size]);
                let _ = self.socket.set_nonblocking(true);
                while let Ok(size) = self.socket.recv(&mut buffer) {
                    self.take(&buffer[..size]);
                }
                let _ = self.socket.set_nonblocking(false);
            }
            self.cycle();
        }

        /// The stack once round: frames in, its answers and services, frames out.
        fn cycle(&mut self) {
            self.iface
                .poll(Moment::now(), &mut self.wire, &mut self.sockets);
            self.lease();
            self.listen();
            self.serve();
            self.iface
                .poll(Moment::now(), &mut self.wire, &mut self.sockets);
            for frame in self.wire.to_cable.drain(..) {
                if let Err(error) = self.socket.send_to(&frame, &self.start.switch)
                    && error.kind() != ErrorKind::WouldBlock
                {
                    log::debug!("{} could not send: {error}", self.start.name);
                }
            }
        }

        /// A frame from the cable, for the stack.
        fn take(&mut self, bytes: &[u8]) {
            self.wire.from_cable.push_back(bytes.to_vec());
        }

        fn lease(&mut self) {
            let Some(handle) = self.dhcp else {
                return;
            };
            let event = match self.sockets.get_mut::<dhcpv4::Socket>(handle).poll() {
                Some(dhcpv4::Event::Configured(config)) => Some((
                    Some(config.address),
                    config.router,
                    config.dns_servers.first().copied(),
                )),
                Some(dhcpv4::Event::Deconfigured) => Some((None, None, None)),
                None => None,
            };
            if let Some((address, gateway, dns)) = event {
                self.settle(address, gateway, dns);
            }
        }

        /// ICMP in: an echo request answered (when it answers ping) through
        /// the raw socket, which waits for ARP as a real stack does; the
        /// rest kept for whoever is waiting on it.
        fn listen(&mut self) {
            let raw = self.raw;
            let me = self.address.map(|a| a.address());
            let mut replies = Vec::new();
            let socket = self.sockets.get_mut::<raw::Socket>(raw);
            while let Ok(ip) = socket.recv() {
                let header = usize::from(ip.first().map_or(5, |b| b & 0x0f)) * 4;
                let to = ip
                    .get(16..20)
                    .map(|d| Ipv4Addr::new(d[0], d[1], d[2], d[3]));
                match ip.get(header) {
                    Some(8) if to.is_some() && to == me => {
                        if self.start.conf.ping
                            && let Some(reply) = frame::echo_reply(&ip[header..])
                        {
                            let from = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
                            replies.push(frame::ipv4_packet(
                                to.unwrap_or(from),
                                from,
                                frame::ICMP,
                                &reply,
                            ));
                        }
                    }
                    Some(8) => {}
                    _ => {
                        if self.heard.len() >= 64 {
                            self.heard.pop_front();
                        }
                        self.heard.push_back(ip.to_vec());
                    }
                }
            }
            for reply in replies {
                let _ = socket.send_slice(&reply);
            }
            // The same for ICMPv6.
            let mine = self.addresses6();
            let raw6 = self.raw6;
            let mut replies = Vec::new();
            let mut routes = Vec::new();
            let socket = self.sockets.get_mut::<raw::Socket>(raw6);
            while let Ok(ip) = socket.recv() {
                let Some(icmp) = ip.get(40..) else {
                    continue;
                };
                let (from, to) = (ip6_at(ip, 8), ip6_at(ip, 24));
                match icmp.first() {
                    Some(&frame::ECHO6_REQUEST) if mine.contains(&to) => {
                        if self.start.conf.ping
                            && let Some(reply) = frame::echo6_reply(icmp)
                        {
                            replies.push(frame::icmpv6_packet(to, from, 64, reply));
                        }
                    }
                    // ponytail: smoltcp's SLAAC adds no IPv6 route once an
                    // IPv4 one is in its table (0.14), so the way out is
                    // taken from the advertisement here; drop this when it
                    // does.
                    Some(&frame::ROUTER_ADVERT) if frame::advertised(icmp).is_some() => {
                        routes.push(from);
                    }
                    Some(kind) if frame::is_discovery(*kind) => {}
                    _ => {
                        if self.heard.len() >= 64 {
                            self.heard.pop_front();
                        }
                        self.heard.push_back(ip.to_vec());
                    }
                }
            }
            for reply in replies {
                let _ = socket.send_slice(&reply);
            }
            for router in routes {
                let _ = self.iface.routes_mut().add_default_ipv6_route(router);
            }
        }

        /// Its IPv6 addresses, the one it made from the router's
        /// advertisement first and its link-local one last.
        fn addresses6(&self) -> Vec<Ipv6Addr> {
            let mut all: Vec<Ipv6Addr> = self
                .iface
                .ip_addrs()
                .iter()
                .filter_map(|cidr| match cidr.address() {
                    IpAddress::Ipv6(address) => Some(address),
                    IpAddress::Ipv4(_) => None,
                })
                .collect();
            all.sort_by_key(|a| a.segments()[0] & 0xffc0 == 0xfe80);
            all
        }

        /// The address it speaks IPv6 as to the rest of the network: the one
        /// it made from the router's advertisement, when it has one.
        fn global6(&self) -> Option<Ipv6Addr> {
            self.addresses6()
                .into_iter()
                .find(|a| a.segments()[0] & 0xffc0 != 0xfe80)
        }

        fn serve(&mut self) {
            self.serve_ssh();
            if let Some(handle) = self.echo {
                let socket = self.sockets.get_mut::<udp::Socket>(handle);
                let mut buffer = [0u8; 1500];
                while let Ok((size, meta)) = socket.recv_slice(&mut buffer) {
                    let _ = socket.send_slice(&buffer[..size], meta.endpoint);
                }
            }
            if let Some(handle) = self.web {
                let page = format!(
                    "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!doctype html><title>{0}</title><h1>{0}</h1><p>A light server in BeatBox.</p>\n",
                    self.start.name
                );
                let socket = self.sockets.get_mut::<tcp::Socket>(handle);
                if !socket.is_open() {
                    let _ = socket.listen(80);
                } else if socket.may_recv() && socket.can_recv() {
                    let _ = socket.recv(|data| (data.len(), ()));
                    let _ = socket.send_slice(page.as_bytes());
                    socket.close();
                } else if !socket.may_recv() && socket.state() == tcp::State::CloseWait {
                    socket.close();
                }
            }
        }

        /// The SSH port: a connection is taken and let go at once, so the
        /// next can come straight after (no TIME_WAIT to sit out).
        fn serve_ssh(&mut self) {
            if let Some(handle) = self.ssh {
                let socket = self.sockets.get_mut::<tcp::Socket>(handle);
                if !socket.is_open() {
                    let _ = socket.listen(22);
                } else if socket.state() == tcp::State::Established {
                    socket.abort();
                }
            }
        }

        /// Pump until `done` says it has its answer, or `limit` passes.
        fn wait_for<T>(
            &mut self,
            limit: Duration,
            mut done: impl FnMut(&mut Self) -> Option<T>,
        ) -> Option<T> {
            let until = Instant::now() + limit;
            loop {
                if let Some(found) = done(self) {
                    return Some(found);
                }
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return None;
                }
                self.pump(left.min(Duration::from_millis(20)));
            }
        }

        /* ── the terminal ─────────────────────────────────────────────────── */

        fn command(&mut self, line: &str) -> Vec<Value> {
            let mut words = line.split_whitespace();
            let Some(verb) = words.next() else {
                return Vec::new();
            };
            let rest: Vec<&str> = words.collect();
            let target = rest.first().copied();
            match (verb, target) {
                ("ipconfig" | "ifconfig" | "dhclient", Some("/renew" | "renew" | "-r"))
                | ("dhclient", None) => self.renew(),
                ("ipconfig" | "ifconfig" | "ip", _) => self.ipconfig(),
                ("ping", Some(host)) => self.ping(host),
                ("traceroute" | "tracert", Some(host)) => self.traceroute(host),
                ("nslookup" | "host" | "dig", Some(_)) => self.nslookup(&rest),
                ("curl" | "wget", Some(url)) => self.curl(url),
                ("nc" | "telnet", Some(host)) => match rest.get(1).and_then(|p| p.parse().ok()) {
                    Some(port) => self.nc(host, port),
                    None => vec![json!({"code": "needs", "command": verb})],
                },
                ("ssh", Some(host)) => self.ssh(host, rest.get(1).copied()),
                ("help" | "?" | "ajuda", _) => vec![json!({"code": "help"})],
                (
                    "ping" | "traceroute" | "tracert" | "nslookup" | "curl" | "ssh" | "nc"
                    | "telnet",
                    None,
                ) => {
                    vec![json!({"code": "needs", "command": verb})]
                }
                _ => vec![json!({"code": "unknown", "command": verb})],
            }
        }

        fn ipconfig(&mut self) -> Vec<Value> {
            let Some(address) = self.address else {
                return vec![json!({"code": "no_address"})];
            };
            vec![json!({
                "code": "ipconfig",
                "address": address.address().to_string(),
                "prefix": address.prefix_len(),
                "gateway": self.gateway.map(|a| a.to_string()),
                "dns": self.dns.map(|a| a.to_string()),
                "address6": self.global6().map(|a| a.to_string()),
                "mac": frame::mac_text(self.start.mac),
                "by": if self.start.conf.dhcp { "dhcp" } else { "fixed" },
            })]
        }

        /// Ask DHCP again, from the start, as `ipconfig /renew` does: the
        /// address it holds is let go, and a new one awaited.
        fn renew(&mut self) -> Vec<Value> {
            let Some(handle) = self.dhcp else {
                return vec![json!({"code": "dhcp.fixed"})];
            };
            self.sockets.get_mut::<dhcpv4::Socket>(handle).reset();
            self.settle(None, None, None);
            let got = self.wait_for(Duration::from_secs(5), |machine| machine.address);
            if got.is_none() {
                return vec![json!({"code": "dhcp.none"})];
            }
            self.ipconfig()
        }

        /// A name to an address: as written, or asked of the DNS server --
        /// its IPv6 address first when this machine speaks IPv6, as a real
        /// system prefers it.
        fn resolve(&mut self, host: &str, said: &mut Vec<Value>) -> Option<IpAddr> {
            if let Ok(address) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
                return Some(address);
            }
            let found = if self.global6().is_some() {
                self.lookup(host, 28).or_else(|| self.lookup(host, 1))
            } else {
                self.lookup(host, 1)
            };
            match found {
                Some(address) => said.push(
                    json!({"code": "resolved", "name": host, "address": address.to_string()}),
                ),
                None => said.push(json!({"code": "dns.none", "name": host})),
            }
            found
        }

        /// `name`'s address of `kind` (1 is A, 28 AAAA).
        fn lookup(&mut self, name: &str, kind: u16) -> Option<IpAddr> {
            let server = self.dns?;
            self.sequence = self.sequence.wrapping_add(1);
            let query = frame::dns_query_for(self.sequence, name, kind);
            let names = self.names;
            let endpoint = IpEndpoint::new(IpAddress::Ipv4(server), 53);
            self.sockets
                .get_mut::<udp::Socket>(names)
                .send_slice(&query, endpoint)
                .ok()?;
            self.wait_for(Duration::from_millis(1500), |machine| {
                let socket = machine.sockets.get_mut::<udp::Socket>(names);
                let mut buffer = [0u8; 1500];
                let (size, _) = socket.recv_slice(&mut buffer).ok()?;
                Some(frame::dns_first(&buffer[..size]).map(|(_, address)| address))
            })
            .flatten()
            .filter(|address| address.is_ipv6() == (kind == 28))
        }

        /// Every record of `kind` the DNS server gives for `name`, CNAMEs on
        /// the way included; `None` when nobody answered.
        fn query(&mut self, name: &str, kind: u16) -> Option<Vec<(u16, String)>> {
            let server = self.dns?;
            self.sequence = self.sequence.wrapping_add(1);
            let question = frame::dns_query_for(self.sequence, name, kind);
            let names = self.names;
            let endpoint = IpEndpoint::new(IpAddress::Ipv4(server), 53);
            self.sockets
                .get_mut::<udp::Socket>(names)
                .send_slice(&question, endpoint)
                .ok()?;
            self.wait_for(Duration::from_millis(1500), |machine| {
                let socket = machine.sockets.get_mut::<udp::Socket>(names);
                let mut buffer = [0u8; 1500];
                let (size, _) = socket.recv_slice(&mut buffer).ok()?;
                Some(frame::dns_records(&buffer[..size]))
            })
        }

        /// `nslookup name`, `nslookup -type=mx name`, `host -t mx name`,
        /// `dig name mx`: its addresses, or the records of the type asked.
        fn nslookup(&mut self, words: &[&str]) -> Vec<Value> {
            if self.dns.is_none() {
                return vec![json!({"code": "no_address"})];
            }
            let kind_of = |word: &str| match word.to_ascii_lowercase().as_str() {
                "a" => Some(1),
                "aaaa" => Some(28),
                "cname" => Some(5),
                "mx" => Some(15),
                "txt" => Some(16),
                _ => None,
            };
            let mut kind = None;
            let mut name = None;
            let mut words = words.iter();
            while let Some(word) = words.next() {
                if let Some(asked) = word
                    .strip_prefix("-type=")
                    .or_else(|| word.strip_prefix("-query="))
                    .or_else(|| word.strip_prefix("-q="))
                {
                    kind = kind_of(asked);
                } else if *word == "-t" {
                    kind = words.next().and_then(|w| kind_of(w));
                } else if word.starts_with('-') {
                } else if name.is_none() {
                    name = Some(*word);
                } else {
                    kind = kind_of(word).or(kind);
                }
            }
            let Some(name) = name else {
                return vec![json!({"code": "needs", "command": "nslookup"})];
            };
            let kinds: &[u16] = match &kind {
                Some(kind) => std::slice::from_ref(kind),
                None => &[1, 28],
            };
            let mut said = Vec::new();
            for kind in kinds {
                for (found, value) in self.query(name, *kind).unwrap_or_default() {
                    let line = match found {
                        1 | 28 => json!({"code": "dns.answer", "name": name, "address": value}),
                        5 => json!({"code": "dns.alias", "name": name, "to": value}),
                        15 => {
                            json!({"code": "dns.record", "name": name, "type": "MX", "value": value})
                        }
                        16 => {
                            json!({"code": "dns.record", "name": name, "type": "TXT", "value": value})
                        }
                        _ => continue,
                    };
                    // An alias is said once, however many questions led to it.
                    if !said.contains(&line) {
                        said.push(line);
                    }
                }
            }
            if said.is_empty() {
                return vec![json!({"code": "dns.none", "name": name})];
            }
            said
        }

        /// One echo request with `ttl` hops to live; what came back and when:
        /// the reply's source, the time, and whether it was the end (a reply)
        /// or a router on the way (an error).
        fn probe(
            &mut self,
            to: IpAddr,
            ttl: u8,
            limit: Duration,
        ) -> Option<(IpAddr, Duration, Answer)> {
            self.sequence = self.sequence.wrapping_add(1);
            let sequence = self.sequence;
            let (raw, packet) = match to {
                IpAddr::V4(to) => {
                    let from = self.address?.address();
                    let mut packet = frame::ipv4_packet(
                        from,
                        to,
                        frame::ICMP,
                        &frame::echo_request(IDENT, sequence),
                    );
                    frame::with_ttl(&mut packet, ttl);
                    (self.raw, packet)
                }
                IpAddr::V6(to) => {
                    let from = if to.segments()[0] & 0xffc0 == 0xfe80 {
                        frame::link_local(self.start.mac)
                    } else {
                        self.global6()?
                    };
                    let echo = frame::echo6_request(IDENT, sequence);
                    (self.raw6, frame::icmpv6_packet(from, to, ttl, echo))
                }
            };
            self.sockets
                .get_mut::<raw::Socket>(raw)
                .send_slice(&packet)
                .ok()?;
            let sent = Instant::now();
            self.wait_for(limit, |machine| {
                let _ = raw;
                let found = machine
                    .heard
                    .iter()
                    .position(|ip| read_icmp(ip, sequence).is_some())?;
                let ip = machine.heard.remove(found)?;
                read_icmp(&ip, sequence).map(|(from, answer)| (from, sent.elapsed(), answer))
            })
        }

        fn ping(&mut self, host: &str) -> Vec<Value> {
            let mut said = Vec::new();
            if self.address.is_none() {
                return vec![json!({"code": "no_address"})];
            }
            let Some(to) = self.resolve(host, &mut said) else {
                return said;
            };
            let mut received = 0;
            // The first may wait for ARP, which smoltcp spaces a second apart.
            for limit in [2000, 1000, 1000] {
                match self.probe(to, 64, Duration::from_millis(limit)) {
                    Some((from, time, Answer::Reply(ttl))) => {
                        received += 1;
                        said.push(json!({"code": "ping.reply", "from": from.to_string(), "ms": time.as_millis().max(1), "ttl": ttl}));
                    }
                    Some((from, _, Answer::Unreachable)) => {
                        said.push(json!({"code": "ping.unreachable", "from": from.to_string()}));
                    }
                    Some((from, _, Answer::Expired)) => {
                        said.push(json!({"code": "ping.expired", "from": from.to_string()}));
                    }
                    None => said.push(json!({"code": "ping.timeout"})),
                }
            }
            said.push(json!({"code": "ping.summary", "sent": 3, "received": received, "to": to.to_string()}));
            said
        }

        fn traceroute(&mut self, host: &str) -> Vec<Value> {
            let mut said = Vec::new();
            if self.address.is_none() {
                return vec![json!({"code": "no_address"})];
            }
            let Some(to) = self.resolve(host, &mut said) else {
                return said;
            };
            for hop in 1..=8u8 {
                let limit = if hop == 1 { 2000 } else { 800 };
                match self.probe(to, hop, Duration::from_millis(limit)) {
                    Some((from, time, answer)) => {
                        said.push(json!({"code": "trace.hop", "hop": hop, "from": from.to_string(), "ms": time.as_millis().max(1)}));
                        if !matches!(answer, Answer::Expired) {
                            break;
                        }
                    }
                    None => {
                        said.push(json!({"code": "trace.silent", "hop": hop, "to": to.to_string()}))
                    }
                }
            }
            said
        }

        /// SSH to `host`, once or `30x`: whether each connection opened, was
        /// refused, or met silence. Nothing is said over it.
        fn ssh(&mut self, host: &str, times: Option<&str>) -> Vec<Value> {
            let mut said = Vec::new();
            if self.address.is_none() {
                return vec![json!({"code": "no_address"})];
            }
            let times = times
                .and_then(|t| t.trim_end_matches(['x', 'X']).parse::<u32>().ok())
                .unwrap_or(1)
                .clamp(1, 100);
            let Some(to) = self.resolve(host, &mut said) else {
                return said;
            };
            let began = Instant::now();
            let (mut open, mut refused, mut silent) = (0, 0, 0);
            for attempt in 0..times {
                // The first may wait for ARP, which smoltcp spaces a second apart.
                let limit = Duration::from_millis(if attempt == 0 { 2000 } else { 1000 });
                match self.knock(to, 22, limit) {
                    "open" => open += 1,
                    "refused" => refused += 1,
                    _ => silent += 1,
                }
            }
            let address = to.to_string();
            said.push(if times == 1 {
                let code = if open == 1 {
                    "ssh.open"
                } else if refused == 1 {
                    "ssh.refused"
                } else {
                    "ssh.silent"
                };
                json!({"code": code, "host": host, "to": address})
            } else {
                json!({"code": "ssh.many", "host": host, "to": address, "tries": times,
                       "open": open, "refused": refused, "silent": silent,
                       "seconds": began.elapsed().as_secs()})
            });
            said
        }

        /// One TCP connection to `to`:`port`: `open`, `refused` (a reset,
        /// or a firewall's "unreachable") or `silent`.
        fn knock(&mut self, to: IpAddr, port: u16, limit: Duration) -> &'static str {
            self.port = if self.port >= 65000 {
                49152
            } else {
                self.port + 1
            };
            let local = self.port;
            let handle = self.sockets.add(tcp_socket());
            let connected = self
                .sockets
                .get_mut::<tcp::Socket>(handle)
                .connect(self.iface.context(), (IpAddress::from(to), port), local)
                .is_ok();
            if !connected {
                self.sockets.remove(handle);
                return "silent";
            }
            self.heard.clear();
            let outcome = self
                .wait_for(limit, |machine| {
                    let socket = machine.sockets.get_mut::<tcp::Socket>(handle);
                    match socket.state() {
                        tcp::State::SynSent => machine
                            .heard
                            .iter()
                            .any(|ip| refuses(ip, to, port))
                            .then_some("refused"),
                        tcp::State::Closed => Some("refused"),
                        _ => Some("open"),
                    }
                })
                .unwrap_or("silent");
            self.sockets.get_mut::<tcp::Socket>(handle).abort();
            self.pump(Duration::from_millis(1));
            self.sockets.remove(handle);
            outcome
        }

        /// `nc host port`: whether something there accepts a connection --
        /// and the first line it says, for one that speaks first (an email
        /// server's greeting). Nothing is sent.
        fn nc(&mut self, host: &str, port: u16) -> Vec<Value> {
            let mut said = Vec::new();
            if self.address.is_none() {
                return vec![json!({"code": "no_address"})];
            }
            let Some(to) = self.resolve(host, &mut said) else {
                return said;
            };
            let tried = self.nc_at(host, port, to);
            self.or_ipv4(host, to, said, tried, |machine, v4| {
                machine.nc_at(host, port, v4)
            })
        }

        /// What a client does when a name's IPv6 address does not answer:
        /// try its IPv4 one. Most services listen on IPv4 alone, and a
        /// system that gave up at the first address would call them down.
        fn or_ipv4(
            &mut self,
            host: &str,
            to: IpAddr,
            mut said: Vec<Value>,
            tried: Vec<Value>,
            again: impl FnOnce(&mut Self, IpAddr) -> Vec<Value>,
        ) -> Vec<Value> {
            let failed = tried.last().is_some_and(|line| {
                matches!(
                    line["code"].as_str(),
                    Some("curl.refused" | "curl.timeout" | "curl.reset" | "curl.failed")
                )
            });
            let named = host.trim_matches(['[', ']']).parse::<IpAddr>().is_err();
            if failed
                && named
                && to.is_ipv6()
                && let Some(v4) = self.lookup(host, 1)
            {
                said.push(json!({"code": "resolved", "name": host, "address": v4.to_string()}));
                said.extend(again(self, v4));
                return said;
            }
            said.extend(tried);
            said
        }

        /// `nc` to one address.
        fn nc_at(&mut self, host: &str, port: u16, to: IpAddr) -> Vec<Value> {
            let mut said = Vec::new();
            self.port = if self.port >= 65000 {
                49152
            } else {
                self.port + 1
            };
            let local = self.port;
            let handle = self.sockets.add(tcp_socket());
            let connected = self
                .sockets
                .get_mut::<tcp::Socket>(handle)
                .connect(self.iface.context(), (IpAddress::from(to), port), local)
                .is_ok();
            if !connected {
                self.sockets.remove(handle);
                said.push(json!({"code": "curl.failed", "host": host}));
                return said;
            }
            self.heard.clear();
            let started = Instant::now();
            let outcome = self.wait_for(Duration::from_secs(4), |machine| {
                let socket = machine.sockets.get_mut::<tcp::Socket>(handle);
                if socket.state() == tcp::State::Closed
                    || machine.heard.iter().any(|ip| refuses(ip, to, port))
                {
                    return Some(false);
                }
                socket.may_send().then_some(true)
            });
            let ms = started.elapsed().as_millis();
            if outcome == Some(true) {
                let mut first = Vec::new();
                self.wait_for(Duration::from_millis(800), |machine| {
                    let socket = machine.sockets.get_mut::<tcp::Socket>(handle);
                    while socket.can_recv() {
                        let _ = socket.recv(|data| {
                            first.extend_from_slice(data);
                            (data.len(), ())
                        });
                    }
                    (first.contains(&b'\n') || !socket.may_recv()).then_some(())
                });
                let line = String::from_utf8_lossy(&first)
                    .lines()
                    .next()
                    .map(|line| line.trim().chars().take(120).collect::<String>())
                    .filter(|line| !line.is_empty());
                said.push(
                    json!({"code": "tcp.open", "host": host, "port": port, "ms": ms, "said": line}),
                );
            }
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);
            socket.abort();
            self.pump(Duration::from_millis(1));
            self.sockets.remove(handle);
            match outcome {
                Some(true) => {}
                Some(false) => {
                    said.push(json!({"code": "curl.refused", "host": host, "port": port}))
                }
                None => said.push(json!({"code": "curl.timeout", "host": host, "port": port})),
            }
            said
        }

        fn curl(&mut self, url: &str) -> Vec<Value> {
            let mut said = Vec::new();
            if self.address.is_none() {
                return vec![json!({"code": "no_address"})];
            }
            let rest = url.trim_start_matches("http://");
            let (authority, path) = rest
                .split_once('/')
                .map_or((rest, "/".to_owned()), |(a, p)| (a, format!("/{p}")));
            let (host, port) = authority
                .rsplit_once(':')
                .and_then(|(h, p)| Some((h, p.parse::<u16>().ok()?)))
                .unwrap_or((authority, 80));
            let Some(to) = self.resolve(host, &mut said) else {
                return said;
            };
            let tried = self.curl_at(host, port, &path, to);
            self.or_ipv4(host, to, said, tried, |machine, v4| {
                machine.curl_at(host, port, &path, v4)
            })
        }

        /// `curl` to one address.
        fn curl_at(&mut self, host: &str, port: u16, path: &str, to: IpAddr) -> Vec<Value> {
            let mut said = Vec::new();
            self.port = if self.port >= 65000 {
                49152
            } else {
                self.port + 1
            };
            let local = self.port;
            let handle = self.sockets.add(tcp_socket());
            let connected = {
                let socket = self.sockets.get_mut::<tcp::Socket>(handle);
                socket
                    .connect(self.iface.context(), (IpAddress::from(to), port), local)
                    .is_ok()
            };
            if !connected {
                self.sockets.remove(handle);
                said.push(json!({"code": "curl.failed", "host": host}));
                return said;
            }
            // What was heard before this connection is not about it.
            self.heard.clear();
            let request =
                format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: beatbox-light\r\n\r\n");
            let mut sent = false;
            let mut body = Vec::new();
            let outcome = self.wait_for(Duration::from_secs(4), |machine| {
                let socket = machine.sockets.get_mut::<tcp::Socket>(handle);
                if socket.state() == tcp::State::Closed && !sent {
                    return Some("refused");
                }
                // A firewall that rejects says "unreachable" about our first
                // packet; smoltcp does not read it, so it is read here.
                if !sent && machine.heard.iter().any(|ip| refuses(ip, to, port)) {
                    return Some("refused");
                }
                // Cut after it said what it wanted: a reset, nothing back.
                if socket.state() == tcp::State::Closed && sent && body.is_empty() {
                    return Some("reset");
                }
                if socket.may_send() && !sent {
                    let _ = socket.send_slice(request.as_bytes());
                    sent = true;
                }
                while socket.can_recv() {
                    let _ = socket.recv(|data| {
                        body.extend_from_slice(data);
                        (data.len(), ())
                    });
                }
                (sent && !socket.may_recv()).then_some("done")
            });
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);
            socket.abort();
            self.pump(Duration::from_millis(1));
            self.sockets.remove(handle);
            match outcome {
                Some("refused") => {
                    said.push(json!({"code": "curl.refused", "host": host, "port": port}))
                }
                Some("reset") => said.push(
                    json!({"code": "curl.reset", "host": host, "port": port, "to": to.to_string()}),
                ),
                _ if body.is_empty() => {
                    said.push(json!({"code": "curl.timeout", "host": host, "port": port}))
                }
                _ => {
                    let text = String::from_utf8_lossy(&body);
                    let status = text.lines().next().unwrap_or_default().to_owned();
                    let title = text
                        .split_once("<title>")
                        .and_then(|(_, t)| t.split_once("</title>"))
                        .map(|(t, _)| t.trim().to_owned());
                    said.push(json!({"code": "curl.answer", "status": status, "bytes": body.len(), "title": title}));
                }
            }
            said
        }
    }

    /// What an ICMP message said about one of this machine's probes.
    enum Answer {
        /// The end answered, with the TTL its reply arrived with.
        Reply(u8),
        /// A router on the way said its time was up.
        Expired,
        /// Somebody said there is no way there.
        Unreachable,
    }

    fn ip6_at(ip: &[u8], at: usize) -> Ipv6Addr {
        let octets: [u8; 16] = ip
            .get(at..at + 16)
            .and_then(|o| o.try_into().ok())
            .unwrap_or([0; 16]);
        Ipv6Addr::from(octets)
    }

    /// Whether `ip` is ICMP "unreachable" about a TCP packet sent to `to`:`port`.
    fn refuses(ip: &[u8], to: IpAddr, port: u16) -> bool {
        if let IpAddr::V6(to) = to {
            // ICMPv6 after the 40-byte header, quoting the packet after its 8.
            return ip.first().is_some_and(|b| b >> 4 == 6)
                && ip.get(40) == Some(&frame::ICMPV6_UNREACHABLE)
                && ip.get(48 + 6) == Some(&frame::TCP)
                && ip6_at(ip, 48 + 24) == to
                && ip.get(48 + 42..48 + 44) == Some(&port.to_be_bytes()[..]);
        }
        let IpAddr::V4(to) = to else {
            return false;
        };
        let header = usize::from(ip.first().map_or(0, |b| b & 0x0f)) * 4;
        let Some(icmp) = ip
            .get(header..)
            .filter(|i| i.first() == Some(&frame::ICMP_UNREACHABLE))
        else {
            return false;
        };
        let quoted = icmp.get(8..).unwrap_or_default();
        let inner = usize::from(quoted.first().map_or(0, |b| b & 0x0f)) * 4;
        quoted.get(9) == Some(&frame::TCP)
            && quoted.get(16..20) == Some(&to.octets()[..])
            && quoted.get(inner + 2..inner + 4) == Some(&port.to_be_bytes()[..])
    }

    /// A packet a raw socket handed over, if it answers probe `sequence`:
    /// who sent it, and what it says.
    fn read_icmp(ip: &[u8], sequence: u16) -> Option<(IpAddr, Answer)> {
        let ours = |echo: &[u8]| {
            echo.get(4..8).is_some_and(|id| {
                id[..2] == IDENT.to_be_bytes() && id[2..] == sequence.to_be_bytes()
            })
        };
        if *ip.first()? >> 4 == 6 {
            let (from, hops, icmp) = (ip6_at(ip, 8), *ip.get(7)?, ip.get(40..)?);
            return match *icmp.first()? {
                frame::ECHO6_REPLY if ours(icmp) => Some((from.into(), Answer::Reply(hops))),
                kind @ (frame::ICMPV6_TIME_EXCEEDED | frame::ICMPV6_UNREACHABLE) => {
                    ours(icmp.get(48..)?).then_some((
                        from.into(),
                        if kind == frame::ICMPV6_TIME_EXCEEDED {
                            Answer::Expired
                        } else {
                            Answer::Unreachable
                        },
                    ))
                }
                _ => None,
            };
        }
        read_icmp4(ip, sequence).map(|(from, answer)| (from.into(), answer))
    }

    fn read_icmp4(ip: &[u8], sequence: u16) -> Option<(Ipv4Addr, Answer)> {
        let header = usize::from(*ip.first()? & 0x0f) * 4;
        let from = Ipv4Addr::new(*ip.get(12)?, *ip.get(13)?, *ip.get(14)?, *ip.get(15)?);
        let ttl = *ip.get(8)?;
        let icmp = ip.get(header..)?;
        let ours = |echo: &[u8]| {
            echo.get(4..8).is_some_and(|id| {
                id[..2] == IDENT.to_be_bytes() && id[2..] == sequence.to_be_bytes()
            })
        };
        match *icmp.first()? {
            0 if ours(icmp) => Some((from, Answer::Reply(ttl))),
            kind @ (frame::ICMP_TIME_EXCEEDED | frame::ICMP_UNREACHABLE) => {
                let quoted = icmp.get(8..)?;
                let inner = usize::from(*quoted.first()? & 0x0f) * 4;
                ours(quoted.get(inner..)?).then_some((
                    from,
                    if kind == frame::ICMP_TIME_EXCEEDED {
                        Answer::Expired
                    } else {
                        Answer::Unreachable
                    },
                ))
            }
            _ => None,
        }
    }

    fn udp_socket() -> udp::Socket<'static> {
        udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 8 * 1500]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 8 * 1500]),
        )
    }

    fn tcp_socket() -> tcp::Socket<'static> {
        tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 16 * 1024]),
            tcp::SocketBuffer::new(vec![0; 16 * 1024]),
        )
    }
}

#[cfg(unix)]
pub use machine::Light;
