//! Shell <-> daemon: one local socket, one JSON object per line. Lives here
//! because both sides speak it.
//!
//! ```text
//! -> {"hello": {"contract": 1, "build": "..."}}
//! <- {"hello": {"contract": 1, "build": "...", "pid": 4242}}
//! -> {"id": "r7", "action": "sandbox.refresh", "payload": {"id": "sbx-3f9c21"}}
//! <- {"id": "r7", "envelope": {"ok": true, "data": {...}, "error": null}}
//! <- {"event": "sandbox", "payload": {...}}
//! -> {"id": "r8", "daemon": "status"}
//! <- {"id": "r8", "daemon": {"running": 1, ...}}
//! <- {"refused": {"code": "bad-line", "message": "...", "reason": null}}
//! ```
//!
//! `hello` first, both ways: shell and daemon can be different builds mid-upgrade.
//! Answers carry the request `id` and come in any order (one thread per
//! request, so a refresh never waits behind a save point). `daemon` talks to
//! the process: `status`, `stop-running` (window closing with "stop"), or
//! `exit` (machines keep running for the next daemon). `refused` = unreadable
//! line; the daemon hangs up after it.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{ApiError, CONTRACT_VERSION, Envelope};

/// The socket's file name.
pub const SOCKET: &str = "beatboxd.sock";

#[cfg(windows)]
pub mod pipe;

/// Where the daemon listens, first match wins:
///
/// 1. `BEATBOX_SOCKET` (tests; UNIX socket paths max out at 107 bytes);
/// 2. `$BEATBOX_HOME/run/`, so another home never meets ~/.beatbox's daemon;
/// 3. `$XDG_RUNTIME_DIR/beatbox/` (per-user, in memory);
/// 4. `~/.beatbox/run/`.
///
/// Windows: a named pipe `\\.\pipe\beatboxd-<hash of the above>` (or
/// `BEATBOX_SOCKET` as is if it's already a pipe). Another home, another daemon.
pub fn socket_path() -> PathBuf {
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let chosen = if let Some(explicit) = set("BEATBOX_SOCKET") {
        PathBuf::from(explicit)
    } else if let Some(home) = set("BEATBOX_HOME") {
        PathBuf::from(home).join("run").join(SOCKET)
    } else if let Some(runtime) = set("XDG_RUNTIME_DIR").filter(|_| cfg!(unix)) {
        PathBuf::from(runtime).join("beatbox").join(SOCKET)
    } else {
        std::env::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".beatbox")
            .join("run")
            .join(SOCKET)
    };
    if cfg!(windows) {
        pipe_name(&chosen)
    } else {
        chosen
    }
}

/// `\\.\pipe\beatboxd-<16 hex>` from `place` (kept if it's already a pipe).
/// FNV-1a over the lowercased path: Windows paths ignore case.
pub fn pipe_name(place: &Path) -> PathBuf {
    const PREFIX: &str = r"\\.\pipe\";
    let text = place.to_string_lossy();
    if text.to_lowercase().starts_with(PREFIX) {
        return place.to_path_buf();
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    PathBuf::from(format!("{PREFIX}beatboxd-{hash:016x}"))
}

/// Who is speaking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    /// `contract_version`; a mismatch means they can't talk.
    pub contract: u32,
    /// Build id; a mismatch means an upgrade is in progress.
    pub build: String,
    /// Daemon's pid; only the daemon sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

impl Hello {
    /// This contract, from `build`.
    pub fn from(build: &str) -> Self {
        Self {
            contract: CONTRACT_VERSION,
            build: build.to_owned(),
            pid: None,
        }
    }
}

/// What the shell sends.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// The first line.
    Hello(Hello),
    /// An action of the contract.
    Action {
        /// Echoed in the answer.
        id: String,
        /// `namespace.verb`.
        action: String,
        /// As the page wrote it.
        payload: Value,
    },
    /// A question for the process itself: `status`, `stop-running`, `exit`.
    Daemon {
        /// Echoed in the answer.
        id: String,
        /// Which.
        what: String,
    },
}

/// What the daemon sends.
#[derive(Debug, Clone, PartialEq)]
pub enum Say {
    /// The first line.
    Hello(Hello),
    /// The answer to an action.
    Envelope {
        /// The request's.
        id: String,
        /// `contract.json`'s own.
        envelope: Envelope,
    },
    /// The answer to a `daemon` question.
    Daemon {
        /// The request's.
        id: String,
        /// What the process says.
        answer: Value,
    },
    /// Something the page did not ask about.
    Event {
        /// `sandbox`, `job`.
        name: String,
        /// As `contract.json` declares it.
        payload: Value,
    },
    /// A line the daemon would not read. It hangs up after this.
    Refused(ApiError),
}

fn text(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bad(message: impl Into<String>) -> ApiError {
    crate::door::refusal("bad-line", message)
}

impl Ask {
    /// Read one line.
    pub fn parse(line: &str) -> Result<Self, ApiError> {
        let value: Value =
            serde_json::from_str(line).map_err(|error| bad(format!("not JSON: {error}")))?;
        let Value::Object(map) = value else {
            return Err(bad("a line is one JSON object"));
        };
        if let Some(hello) = map.get("hello") {
            return serde_json::from_value(hello.clone())
                .map(Ask::Hello)
                .map_err(|error| bad(format!("not a hello: {error}")));
        }
        let id = text(&map, "id").ok_or_else(|| bad("a request has a string id"))?;
        if let Some(action) = text(&map, "action") {
            let payload = map.get("payload").cloned().unwrap_or(Value::Null);
            return Ok(Ask::Action {
                id,
                action,
                payload,
            });
        }
        if let Some(what) = text(&map, "daemon") {
            return Ok(Ask::Daemon { id, what });
        }
        Err(bad("a request names an action or asks the daemon"))
    }

    /// Write it as one line, without the newline.
    pub fn line(&self) -> String {
        let value = match self {
            Ask::Hello(hello) => json!({"hello": hello}),
            Ask::Action {
                id,
                action,
                payload,
            } => json!({"id": id, "action": action, "payload": payload}),
            Ask::Daemon { id, what } => json!({"id": id, "daemon": what}),
        };
        value.to_string()
    }
}

impl Say {
    /// Read one line.
    pub fn parse(line: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
        let Value::Object(map) = value else {
            return Err("a line is one JSON object".into());
        };
        if let Some(hello) = map.get("hello") {
            return serde_json::from_value(hello.clone())
                .map(Say::Hello)
                .map_err(|error| error.to_string());
        }
        if let Some(refused) = map.get("refused") {
            return serde_json::from_value(refused.clone())
                .map(Say::Refused)
                .map_err(|error| error.to_string());
        }
        if let Some(name) = text(&map, "event") {
            let payload = map.get("payload").cloned().unwrap_or(Value::Null);
            return Ok(Say::Event { name, payload });
        }
        let id = text(&map, "id").ok_or("an answer has a string id")?;
        if let Some(envelope) = map.get("envelope") {
            return serde_json::from_value(envelope.clone())
                .map(|envelope| Say::Envelope { id, envelope })
                .map_err(|error| error.to_string());
        }
        if let Some(answer) = map.get("daemon") {
            return Ok(Say::Daemon {
                id,
                answer: answer.clone(),
            });
        }
        Err("an answer carries an envelope or the daemon's".into())
    }

    /// Write it as one line, without the newline.
    pub fn line(&self) -> String {
        let value = match self {
            Say::Hello(hello) => json!({"hello": hello}),
            Say::Envelope { id, envelope } => json!({"id": id, "envelope": envelope}),
            Say::Daemon { id, answer } => json!({"id": id, "daemon": answer}),
            Say::Event { name, payload } => json!({"event": name, "payload": payload}),
            Say::Refused(error) => json!({"refused": error}),
        };
        value.to_string()
    }
}

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;

#[cfg(windows)]
type Stream = pipe::PipeStream;

#[cfg(unix)]
fn open(path: &Path, _within: Duration) -> io::Result<Stream> {
    Stream::connect(path)
}

#[cfg(windows)]
fn open(path: &Path, within: Duration) -> io::Result<Stream> {
    pipe::connect(path, within)
}

type Waiting = Arc<Mutex<HashMap<String, mpsc::Sender<Say>>>>;

/// A blocking, shareable connection to the daemon. One reader thread
/// matches answers by id; events go to [`Client::events`].
pub struct Client {
    writer: Mutex<Box<dyn Write + Send>>,
    hang_up: Box<dyn Fn() + Send + Sync>,
    waiting: Waiting,
    alive: Arc<std::sync::atomic::AtomicBool>,
    events: Mutex<Option<mpsc::Receiver<(String, Value)>>>,
    next: AtomicU64,
    theirs: Hello,
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "beatboxd hung up")
}

/// Never sent, so safe to retry elsewhere. Unlike [`gone`]: sent, never answered.
fn unsent() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "beatboxd is not connected")
}

impl Client {
    /// Connect to the daemon at `path`, say hello as `ours`, and wait up to
    /// `timeout` for its hello.
    pub fn connect(path: &Path, ours: &Hello, timeout: Duration) -> io::Result<Self> {
        let stream = open(path, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        let mut reader = std::io::BufReader::new(stream.try_clone()?);
        let mut writer = stream.try_clone()?;
        writeln!(writer, "{}", Ask::Hello(ours.clone()).line())?;
        let mut first = String::new();
        if reader.read_line(&mut first)? == 0 {
            return Err(gone());
        }
        let theirs = match Say::parse(first.trim_end()) {
            Ok(Say::Hello(hello)) => hello,
            Ok(Say::Refused(error)) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("{}: {}", error.code, error.message),
                ));
            }
            Ok(other) => return Err(io::Error::other(format!("not a hello: {other:?}"))),
            Err(error) => return Err(io::Error::other(error)),
        };
        stream.set_read_timeout(None)?;
        let closer = stream.try_clone()?;
        let hang_up = Box::new(move || {
            let _ = closer.shutdown(std::net::Shutdown::Both);
        });
        Ok(Self::around(reader, Box::new(writer), hang_up, theirs))
    }

    fn around(
        mut reader: impl BufRead + Send + 'static,
        writer: Box<dyn Write + Send>,
        hang_up: Box<dyn Fn() + Send + Sync>,
        theirs: Hello,
    ) -> Self {
        let waiting: Waiting = Arc::default();
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (tell, events) = mpsc::channel();
        let answers = Arc::clone(&waiting);
        let living = Arc::clone(&alive);
        thread::Builder::new()
            .name("beatbox-wire".into())
            .spawn(move || {
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let said = match Say::parse(line.trim_end()) {
                        Ok(said) => said,
                        Err(_) => continue,
                    };
                    let id = match &said {
                        Say::Envelope { id, .. } | Say::Daemon { id, .. } => id.clone(),
                        Say::Event { name, payload } => {
                            let _ = tell.send((name.clone(), payload.clone()));
                            continue;
                        }
                        Say::Refused(_) => break,
                        Say::Hello(_) => continue,
                    };
                    let waiter = answers
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&id);
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(said);
                    }
                }
                // Wake every waiter, and under this lock so nobody starts waiting after.
                let mut waiting = answers.lock().unwrap_or_else(PoisonError::into_inner);
                living.store(false, Ordering::SeqCst);
                waiting.clear();
            })
            .expect("a thread can be started");
        Self {
            writer: Mutex::new(writer),
            hang_up,
            waiting,
            alive,
            events: Mutex::new(Some(events)),
            next: AtomicU64::new(1),
            theirs,
        }
    }

    /// False once either side hung up.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// What the daemon said about itself.
    pub fn theirs(&self) -> &Hello {
        &self.theirs
    }

    /// The event stream. Only the first caller gets it.
    pub fn events(&self) -> Option<mpsc::Receiver<(String, Value)>> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn ask(&self, make: impl FnOnce(String) -> Ask) -> io::Result<Say> {
        let id = format!("r{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (answer, answered) = mpsc::channel();
        {
            // Same lock the reader clears on exit, so nobody waits on a dead connection.
            let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
            if !self.alive() {
                return Err(unsent());
            }
            waiting.insert(id.clone(), answer);
        }
        let line = make(id.clone()).line();
        {
            let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
            if writeln!(writer, "{line}")
                .and_then(|()| writer.flush())
                .is_err()
            {
                self.waiting
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                return Err(unsent());
            }
        }
        answered.recv().map_err(|_| gone())
    }

    /// Run one action of the contract.
    pub fn call(&self, action: &str, payload: Value) -> io::Result<Envelope> {
        match self.ask(|id| Ask::Action {
            id,
            action: action.to_owned(),
            payload,
        })? {
            Say::Envelope { envelope, .. } => Ok(envelope),
            other => Err(io::Error::other(format!("not an envelope: {other:?}"))),
        }
    }

    /// Ask the process itself.
    pub fn daemon(&self, what: &str) -> io::Result<Value> {
        match self.ask(|id| Ask::Daemon {
            id,
            what: what.to_owned(),
        })? {
            Say::Daemon { answer, .. } => Ok(answer),
            other => Err(io::Error::other(format!("not the daemon's: {other:?}"))),
        }
    }
}

/// Shut the socket down explicitly: the reader thread holds a clone, and the
/// daemon counts the window as connected until both are closed.
impl Drop for Client {
    fn drop(&mut self) {
        (self.hang_up)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_reads_back_as_it_was_written() {
        let asks = [
            Ask::Hello(Hello::from("test")),
            Ask::Action {
                id: "r1".into(),
                action: "sandbox.view".into(),
                payload: json!({"id": "sbx-1"}),
            },
            Ask::Daemon {
                id: "r2".into(),
                what: "status".into(),
            },
        ];
        for ask in asks {
            assert_eq!(Ask::parse(&ask.line()).unwrap(), ask);
        }
        let says = [
            Say::Hello(Hello {
                pid: Some(7),
                ..Hello::from("test")
            }),
            Say::Envelope {
                id: "r1".into(),
                envelope: crate::door::answered(json!([])),
            },
            Say::Daemon {
                id: "r2".into(),
                answer: json!({"running": 0}),
            },
            Say::Event {
                name: "sandbox".into(),
                payload: json!({"sequence": 1}),
            },
            Say::Refused(bad("no")),
        ];
        for say in says {
            assert_eq!(Say::parse(&say.line()).unwrap(), say);
        }
    }

    #[test]
    fn a_line_that_is_not_a_request_is_refused_by_name() {
        for line in [
            "",
            "[]",
            "{}",
            r#"{"id": 1, "action": "x"}"#,
            r#"{"id": "r1"}"#,
        ] {
            assert_eq!(Ask::parse(line).unwrap_err().code, "bad-line", "{line}");
        }
        assert_eq!(
            Ask::parse(r#"{"hello": {"contract": 1}}"#)
                .unwrap_err()
                .code,
            "bad-line"
        );
    }
}
