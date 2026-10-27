//! One connection: hello, then requests and events until someone hangs up.
//!
//! Two threads per connection (read, write) plus one per request, so a
//! refresh never queues behind a save point on another machine.
//!
//! Never block the bus: events are queued without waiting, because every
//! lane may be waiting on that delivery. A window whose queue fills up gets
//! hung up on instead of growing memory.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use beatbox_api::Api;
use beatbox_contract::wire::{Ask, Hello, Say};
use beatbox_contract::{CONTRACT_VERSION, door};
use serde_json::{Value, json};

/// Max lines queued per connection before we hang up.
const QUEUE: usize = 1024;

/// How long a new connection has to say hello.
const HELLO_WITHIN: Duration = Duration::from_secs(10);

/// With no window connected, re-check machines this often: catches guests
/// that died or shut down, so an idle daemon can exit.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// One connection, whatever carries it: a UNIX socket, or a named pipe.
pub trait Conn: Read + Write + Send + Sync + Sized + 'static {
    /// The same connection, for another thread.
    fn duplicate(&self) -> io::Result<Self>;
    /// Hang up, so that every read on it ends.
    fn hang_up(&self);
    /// How long a read may wait.
    fn read_deadline(&self, within: Option<Duration>) -> io::Result<()>;
}

#[cfg(unix)]
impl Conn for std::os::unix::net::UnixStream {
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn hang_up(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }

    fn read_deadline(&self, within: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(within)
    }
}

#[cfg(windows)]
impl Conn for beatbox_contract::wire::pipe::PipeStream {
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn hang_up(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }

    fn read_deadline(&self, within: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(within)
    }
}

/// The daemon, as its connections share it.
pub struct Server {
    pub api: Arc<Api>,
    pub build: String,
    pub socket: PathBuf,
    pub idle: Duration,
    connections: AtomicUsize,
    quiet_since: Mutex<Instant>,
}

enum Out {
    Line(String),
    /// Signals once this and everything before it is written.
    Flushed(mpsc::Sender<()>),
}

impl Server {
    pub fn new(api: Arc<Api>, build: String, socket: PathBuf, idle: Duration) -> Arc<Self> {
        Arc::new(Self {
            api,
            build,
            socket,
            idle,
            connections: AtomicUsize::new(0),
            quiet_since: Mutex::new(Instant::now()),
        })
    }

    fn hello(&self) -> Hello {
        Hello {
            pid: Some(std::process::id()),
            ..Hello::from(&self.build)
        }
    }

    /// Go, leaving every machine running for whoever comes next.
    pub fn leave(&self, why: &str) -> ! {
        let _ = std::fs::remove_file(&self.socket);
        let running = self.api.running().len();
        log::info!("{why}; exiting, and leaving {running} machine(s) as they are");
        log::logger().flush();
        std::process::exit(0)
    }

    fn touch(&self) {
        *self
            .quiet_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    /// Needed while there's a window, a running/frozen machine, an install or a download.
    fn needed(&self) -> bool {
        self.connections.load(Ordering::SeqCst) > 0
            || !self.api.running().is_empty()
            || self.api.building()
            || self
                .api
                .jobs()
                .current()
                .is_some_and(|job| job.state == beatbox_api::jobs::RUNNING)
    }

    fn status(&self) -> Value {
        json!({
            "build": self.build,
            "contract": CONTRACT_VERSION,
            "pid": std::process::id(),
            "owner": self.api.owns(),
            "connections": self.connections.load(Ordering::SeqCst),
            "running": self.api.running().len(),
            "installing": self.api.building(),
            "idle": self.idle.as_secs(),
        })
    }

    /// Exit once nothing has needed this process for `idle`.
    pub fn watch_idle(self: &Arc<Self>) {
        let server = Arc::clone(self);
        thread::Builder::new()
            .name("beatboxd-idle".into())
            .spawn(move || {
                let mut looked = Instant::now();
                loop {
                    thread::sleep(Duration::from_millis(250));
                    let alone = server.connections.load(Ordering::SeqCst) == 0;
                    if alone && looked.elapsed() >= LOOK_EVERY {
                        looked = Instant::now();
                        for sandbox in server.api.running() {
                            let _ = sandbox.refresh();
                        }
                    }
                    if server.needed() {
                        server.touch();
                        continue;
                    }
                    let quiet = server
                        .quiet_since
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .elapsed();
                    if quiet >= server.idle {
                        server.leave("nothing has needed this daemon");
                    }
                }
            })
            .expect("a thread can be started");
    }

    /// Serve every connection from `next`, hanging up on any `ours` rejects.
    pub fn accept<C: Conn>(
        self: &Arc<Self>,
        mut next: impl FnMut() -> io::Result<C>,
        ours: impl Fn(&C) -> bool,
    ) {
        loop {
            let stream = match next() {
                Ok(stream) => stream,
                Err(error) => {
                    log::warn!("a connection failed to arrive: {error}");
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            if !ours(&stream) {
                stream.hang_up();
                log::warn!("hung up on a process of another user");
                continue;
            }
            self.connections.fetch_add(1, Ordering::SeqCst);
            self.touch();
            let server = Arc::clone(self);
            let spawned = thread::Builder::new()
                .name("beatboxd-read".into())
                .spawn(move || {
                    if let Err(error) = server.converse(stream) {
                        log::debug!("a connection ended: {error}");
                    }
                    server.touch();
                    server.connections.fetch_sub(1, Ordering::SeqCst);
                });
            if spawned.is_err() {
                self.connections.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    fn converse<C: Conn>(self: &Arc<Self>, stream: C) -> io::Result<()> {
        let (out, queue) = mpsc::sync_channel::<Out>(QUEUE);
        let mut writer = stream.duplicate()?;
        thread::Builder::new()
            .name("beatboxd-write".into())
            .spawn(move || {
                for item in queue {
                    match item {
                        Out::Line(line) => {
                            if writeln!(writer, "{line}").is_err() {
                                writer.hang_up();
                                break;
                            }
                        }
                        Out::Flushed(done) => {
                            let _ = writer.flush();
                            let _ = done.send(());
                        }
                    }
                }
            })?;
        let say = |said: Say| {
            let _ = out.send(Out::Line(said.line()));
        };
        let hang_up = |error| {
            say(Say::Refused(error));
            let (done, flushed) = mpsc::channel();
            let _ = out.send(Out::Flushed(done));
            let _ = flushed.recv_timeout(Duration::from_secs(1));
            stream.hang_up();
        };

        stream.read_deadline(Some(HELLO_WITHIN))?;
        let mut reader = BufReader::new(stream.duplicate()?);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let theirs = match Ask::parse(line.trim_end()) {
            Ok(Ask::Hello(theirs)) => theirs,
            Ok(_) => {
                hang_up(door::refusal("bad-line", "the first line is a hello"));
                return Ok(());
            }
            Err(error) => {
                hang_up(error);
                return Ok(());
            }
        };
        stream.read_deadline(None)?;
        say(Say::Hello(self.hello()));
        // Different contract = upgrade in progress, not an error: it can still
        // ask status or exit; actions get refused with the reason.
        let understood = theirs.contract == CONTRACT_VERSION;
        if !understood {
            log::warn!(
                "a window speaks contract {}, this daemon {CONTRACT_VERSION}",
                theirs.contract
            );
        }
        if theirs.build != self.build {
            log::info!(
                "a window from build {} (this is {})",
                theirs.build,
                self.build
            );
        }

        let events = out.clone();
        let full = stream.duplicate()?;
        let subscription = self.api.hub().subscribe(move |name, payload| {
            let line = Say::Event {
                name: name.to_owned(),
                payload: payload.clone(),
            }
            .line();
            if let Err(TrySendError::Full(_)) = events.try_send(Out::Line(line)) {
                log::warn!("a window stopped reading its events; hanging up on it");
                full.hang_up();
            }
        });

        let ended = loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break Ok(()),
                Ok(_) => {}
                Err(error) => break Err(error),
            }
            match Ask::parse(line.trim_end()) {
                Ok(Ask::Action {
                    id,
                    action,
                    payload,
                }) => self.answer(&out, id, action, payload, theirs.contract),
                Ok(Ask::Daemon { id, what }) => self.answer_daemon(&out, id, &what),
                Ok(Ask::Hello(_)) => {
                    hang_up(door::refusal("bad-line", "hello was already said"));
                    break Ok(());
                }
                Err(error) => {
                    hang_up(error);
                    break Ok(());
                }
            }
        };
        self.api.hub().unsubscribe(subscription);
        ended
    }

    /// One action, on a thread of its own.
    fn answer(
        self: &Arc<Self>,
        out: &SyncSender<Out>,
        id: String,
        action: String,
        payload: Value,
        contract: u32,
    ) {
        let understood = contract == CONTRACT_VERSION;
        let server = Arc::clone(self);
        let out = out.clone();
        let spawned = thread::Builder::new()
            .name("beatboxd-ask".into())
            .spawn(move || {
                let envelope = if understood {
                    server.api.dispatch(&action, &payload)
                } else {
                    door::refused(beatbox_api::wrong_contract(CONTRACT_VERSION, contract))
                };
                let _ = out.send(Out::Line(Say::Envelope { id, envelope }.line()));
            });
        if let Err(error) = spawned {
            log::error!("could not answer a request: {error}");
        }
    }

    fn answer_daemon(self: &Arc<Self>, out: &SyncSender<Out>, id: String, what: &str) {
        let reply = |answer: Value| {
            let _ = out.send(Out::Line(
                Say::Daemon {
                    id: id.clone(),
                    answer,
                }
                .line(),
            ));
        };
        match what {
            "status" => reply(self.status()),
            "stop-running" => {
                let server = Arc::clone(self);
                let out = out.clone();
                let id = id.clone();
                thread::spawn(move || {
                    let stubborn = server.api.stop_running();
                    let answer = json!({"stubborn": stubborn});
                    let _ = out.send(Out::Line(Say::Daemon { id, answer }.line()));
                });
            }
            "exit" => {
                reply(json!({"exiting": true, "running": self.api.running().len()}));
                let (done, flushed) = mpsc::channel();
                let _ = out.send(Out::Flushed(done));
                let _ = flushed.recv_timeout(Duration::from_secs(2));
                self.leave("asked to go");
            }
            other => reply(json!({"error": format!("beatboxd does not answer {other:?}")})),
        }
    }
}
