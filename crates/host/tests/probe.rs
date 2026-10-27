//! `network.probe`'s Linux scripts, run for real by bash on this machine
//! against a web server and a DNS server written right here.
//!
//! The unit tests in `src/probe.rs` hold what is sent and how the answer is
//! read; this holds that bash really says it. Linux only: the scripts are
//! the Linux guest's (`/dev/tcp`, `/dev/udp`, `date +%s%N`).
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::process::Command;
use std::time::Duration;

use beatbox_engine::{Data, data};
use beatbox_host::probe::Ask;

fn run(arguments: Data) -> Data {
    let ask = Ask::from(&arguments).unwrap();
    let (program, words) = ask.argv(false, "unused");
    let out = Command::new(program).args(words).output().unwrap();
    ask.read(&String::from_utf8_lossy(&out.stdout))
}

fn have_bash() -> bool {
    Command::new("bash")
        .arg("-c")
        .arg("true")
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn tcp_and_http_from_bash() {
    if !have_bash() {
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        // The TCP probe only connects; it sends no HTTP request.
        let (stream, _) = listener.accept().unwrap();
        drop(stream);
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 8192, "HTTP request headers are too large");
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        // Closing with unread request bytes can reset the connection and
        // discard the response. TCP reads need not contain all the headers.
        assert!(request.starts_with(b"GET / HTTP/1.0\r\n"));
        stream
            .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .unwrap();
    });
    let tcp = run(data!({"kind": "tcp", "address": "127.0.0.1", "port": port}));
    assert_eq!(tcp["ok"], true, "{tcp:?}");
    let http = run(data!({"kind": "http", "address": "127.0.0.1", "port": port}));
    assert_eq!(http["said"], "200 OK", "{http:?}");
    assert_eq!(http["ok"], true);
    server.join().unwrap();

    // Nothing listening any more.
    let closed = run(data!({"kind": "tcp", "address": "127.0.0.1", "port": port}));
    assert_eq!(closed["ok"], false, "{closed:?}");
}

/// A DNS answer to `query`: `address` for it, or NXDOMAIN when there is none.
fn answer(query: &[u8], address: Option<[u8; 4]>) -> Vec<u8> {
    let question_end = 12 + query[12..].iter().position(|b| *b == 0).unwrap() + 1 + 4;
    let mut out = query[..2].to_vec();
    out.extend_from_slice(&[0x81, if address.is_some() { 0x80 } else { 0x83 }]);
    out.extend_from_slice(&[0, 1, 0, u8::from(address.is_some()), 0, 0, 0, 0]);
    out.extend_from_slice(&query[12..question_end]);
    if let Some(ip) = address {
        out.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        out.extend_from_slice(&ip);
    }
    out
}

#[test]
fn dns_from_bash() {
    if !have_bash() {
        return;
    }
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut asked = Vec::new();
        for _ in 0..2 {
            let mut buffer = [0u8; 512];
            let (size, from) = socket.recv_from(&mut buffer).unwrap();
            let query = &buffer[..size];
            // The name, as labels: \x08intranet\x03lab\x00.
            let known = query.windows(13).any(|w| w == b"\x08intranet\x03lab");
            asked.push(known);
            let reply = answer(query, known.then_some([10, 50, 20, 11]));
            socket.send_to(&reply, from).unwrap();
        }
        asked
    });
    let found =
        run(data!({"kind": "dns", "address": "127.0.0.1", "port": port, "name": "intranet.lab"}));
    assert_eq!(found["answer"], "10.50.20.11", "{found:?}");
    assert_eq!(found["ok"], true);
    let missing =
        run(data!({"kind": "dns", "address": "127.0.0.1", "port": port, "name": "nowhere.lab"}));
    assert_eq!(missing["said"], "no such name", "{missing:?}");
    assert_eq!(
        server.join().unwrap(),
        [true, false],
        "each question was one datagram, well formed"
    );
}
