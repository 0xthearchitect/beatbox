//! Where QEMU listens for BeatBox: a UNIX socket, or a port on 127.0.0.1.
//!
//! Monitor and agent are UNIX sockets on Linux and loopback ports on Windows.
//! QEMU on Windows takes `-qmp unix:C:\...`, but std has no Windows
//! `AF_UNIX` to connect with.
//! ponytail: loopback TCP on Windows; AF_UNIX there needs a crate or Winsock.
//!
//! **The same path either way.** On Linux `monitor.sock` is the socket; on
//! Windows it's a small file holding `tcp:127.0.0.1:<port>`, so "does this
//! channel exist?" asks the same path everywhere.
//!
//! **What TCP gives up:** a UNIX socket in the user's room is theirs alone;
//! a loopback port is reachable by any local process, and QMP has no
//! password. Bound to loopback only, never a network.
//!
//! `BEATBOX_CONTROL=tcp` uses TCP on Linux too, for testing the Windows path
//! against a real QEMU on a machine that is not Windows.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use crate::host_os::WINDOWS;

/// What a recorded endpoint starts with.
pub const TCP: &str = "tcp:";
/// Where a TCP endpoint listens, and nowhere else.
pub const LOOPBACK: &str = "127.0.0.1";

/// Whether the control channels are loopback ports here.
pub fn use_tcp() -> bool {
    WINDOWS || std::env::var("BEATBOX_CONTROL").as_deref() == Ok("tcp")
}

/// The endpoint QEMU is to listen on for the channel named by `path`.
///
/// On Linux, the path itself. On Windows, a free port on loopback -- written
/// into `path` first, so whoever looks for this channel later finds it.
pub fn prepare(path: &Path) -> io::Result<String> {
    prepare_with(path, use_tcp())
}

/// [`prepare`], with the choice of transport handed in.
pub fn prepare_with(path: &Path, tcp: bool) -> io::Result<String> {
    if !tcp {
        return Ok(path.to_string_lossy().into_owned());
    }
    // Port 0: the system picks one that is free right now. There is a moment
    // between this closing and QEMU binding it; losing that race fails the
    // start with QEMU's own "address in use", out loud.
    let port = TcpListener::bind((LOOPBACK, 0))?.local_addr()?.port();
    let endpoint = format!("{TCP}{LOOPBACK}:{port}");
    fs::write(path, &endpoint)?;
    Ok(endpoint)
}

/// What to connect to for `target`: an endpoint, or the path of a file
/// holding one, or a UNIX socket's own path.
pub fn resolve(target: &Path) -> String {
    let text = target.to_string_lossy().into_owned();
    if text.starts_with(TCP) {
        return text;
    }
    // A regular file is an endpoint record; a UNIX socket is not a regular
    // file, so this is false for one.
    if target.is_file()
        && let Ok(recorded) = fs::read_to_string(target)
    {
        let recorded = recorded.trim();
        if recorded.starts_with(TCP) {
            return recorded.to_owned();
        }
    }
    text
}

/// One open control channel, whichever kind it is.
pub enum Stream {
    /// A port on 127.0.0.1.
    Tcp(TcpStream),
    /// A UNIX socket in the machine's room.
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Stream {
    /// How long a read may wait. `None` for ever; never zero.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let timeout = timeout.map(|t| t.max(Duration::from_millis(1)));
        match self {
            Self::Tcp(stream) => stream.set_read_timeout(timeout),
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_read_timeout(timeout),
        }
    }

    /// How long a write may wait. `None` for ever; never zero.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let timeout = timeout.map(|t| t.max(Duration::from_millis(1)));
        match self {
            Self::Tcp(stream) => stream.set_write_timeout(timeout),
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_write_timeout(timeout),
        }
    }

    /// How long a read waits now.
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        match self {
            Self::Tcp(stream) => stream.read_timeout(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read_timeout(),
        }
    }

    /// Close both directions. Closing one that already went is not an error.
    pub fn shutdown(&self) {
        let _ = match self {
            Self::Tcp(stream) => stream.shutdown(Shutdown::Both),
            #[cfg(unix)]
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
        };
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

/// An open connection to `target`, or the reason there is none.
pub fn connect(target: &Path, timeout: Duration) -> io::Result<Stream> {
    let endpoint = resolve(target);
    let stream = if let Some(address) = endpoint.strip_prefix(TCP) {
        let (host, port) = address
            .rsplit_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no port recorded"))?;
        let port: u16 = port
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not a port"))?;
        let host: std::net::IpAddr = host
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not an address"))?;
        let stream = TcpStream::connect_timeout(
            &SocketAddr::new(host, port),
            timeout.max(Duration::from_millis(1)),
        )?;
        Stream::Tcp(stream)
    } else {
        unix(&endpoint)?
    };
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

#[cfg(unix)]
fn unix(path: &str) -> io::Result<Stream> {
    Ok(Stream::Unix(UnixStream::connect(path)?))
}

#[cfg(not(unix))]
fn unix(path: &str) -> io::Result<Stream> {
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no endpoint recorded at {path}"),
    ))
}

/// The value of `-qmp` that makes QEMU listen on `endpoint`.
pub fn qmp_argument(endpoint: &str) -> String {
    if endpoint.starts_with(TCP) {
        return format!("{endpoint},server=on,wait=off");
    }
    format!("unix:{},server=on,wait=off", option(endpoint))
}

/// The value of `-chardev` for a socket QEMU listens on at `endpoint`.
pub fn chardev_argument(endpoint: &str, chardev_id: &str) -> String {
    if let Some(address) = endpoint.strip_prefix(TCP) {
        let (host, port) = address.rsplit_once(':').unwrap_or((address, ""));
        return format!("socket,host={host},port={port},server=on,wait=off,id={chardev_id}");
    }
    format!(
        "socket,path={},server=on,wait=off,id={chardev_id}",
        option(endpoint)
    )
}

/// `value` made safe inside a QEMU `key=value,key=value` option.
///
/// QEMU splits on commas and reads a doubled one as literal, or
/// `Win10,x64.qcow2` gets cut in two and the rest read as another option.
pub fn option(value: &str) -> String {
    value.replace(',', ",,")
}
