//! The real `beatboxd` binary on a real socket, and once a real guest that
//! outlives two daemons. Each test gets its own BEATBOX_HOME and socket/pipe,
//! so it never touches the developer's own daemon or machines.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use beatbox_contract::wire::{Client, Hello};
use beatbox_contract::{CONTRACT_VERSION, Envelope};
use serde_json::{Value, json};

/// Home + socket + daemon, stopped on drop. Short paths: UNIX sockets cap at
/// 107 bytes and the guest's sockets live under this home.
struct Bench {
    home: PathBuf,
    socket: PathBuf,
}

/// `BEATBOX_DAEMON_BIN` (the Windows run, built elsewhere) or cargo's binary.
fn beatboxd() -> PathBuf {
    std::env::var_os("BEATBOX_DAEMON_BIN").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_beatboxd")),
        PathBuf::from,
    )
}

impl Bench {
    fn new() -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("bbd-{}-{n}", std::process::id()));
        let run = root.join("run");
        fs::create_dir_all(&run).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let socket = if cfg!(windows) {
            PathBuf::from(format!(r"\\.\pipe\bbd-test-{}-{n}", std::process::id()))
        } else {
            run.join("d.sock")
        };
        Self {
            home: root.join("home"),
            socket,
        }
    }

    /// Whether anything still listens where the daemon did.
    fn listening(&self) -> bool {
        #[cfg(windows)]
        {
            beatbox_contract::wire::pipe::exists(&self.socket)
        }
        #[cfg(not(windows))]
        {
            self.socket.exists()
        }
    }

    fn root(&self) -> &Path {
        self.home.parent().unwrap()
    }

    fn start(&self, idle: f64) -> Daemon {
        self.start_on(&self.socket, idle)
    }

    fn start_on(&self, socket: &Path, idle: f64) -> Daemon {
        let child = Command::new(beatboxd())
            .args(["--idle", &idle.to_string(), "--log"])
            .arg(self.root().join("beatboxd.log"))
            .env("BEATBOX_HOME", &self.home)
            .env("BEATBOX_SOCKET", socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Daemon { child }
    }

    fn connect(&self) -> Client {
        self.connect_as(CONTRACT_VERSION)
    }

    fn connect_as(&self, contract: u32) -> Client {
        let deadline = Instant::now() + Duration::from_secs(10);
        let hello = Hello {
            contract,
            ..Hello::from("tests")
        };
        loop {
            match Client::connect(&self.socket, &hello, Duration::from_secs(10)) {
                Ok(client) => return client,
                Err(error) if Instant::now() > deadline => panic!("no daemon answered: {error}"),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root().join("beatboxd.log")).unwrap_or_default()
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        // Kill QEMUs a failed test left; only ours have this folder on the command line.
        let _ = Command::new("pkill")
            .args(["-KILL", "-f"])
            .arg(self.root())
            .status();
        let _ = fs::remove_dir_all(self.root());
    }
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn ended_within(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn data(envelope: Envelope) -> Value {
    assert!(envelope.ok, "{:?}", envelope.error);
    envelope.data.unwrap_or(Value::Null)
}

#[test]
fn a_window_is_greeted_and_answered_in_the_contracts_envelope() {
    let bench = Bench::new();
    let daemon = bench.start(30.0);
    let client = bench.connect();
    assert_eq!(client.theirs().contract, CONTRACT_VERSION);
    assert_eq!(client.theirs().pid, Some(daemon.pid()));
    assert!(client.theirs().build.starts_with(env!("CARGO_PKG_VERSION")));

    let types = data(client.call("sandbox.types", json!({})).unwrap());
    assert_eq!(types[0]["name"], "host");
    let probe = data(client.call("system.probe", json!({})).unwrap());
    let _: beatbox_contract::SystemProbe = serde_json::from_value(probe).unwrap();
    let refused = client.call("files.places", json!({})).unwrap();
    assert_eq!(refused.error.unwrap().code, "not-here");

    let status = client.daemon("status").unwrap();
    assert_eq!(status["owner"], true);
    assert_eq!(status["connections"], 1);
    assert_eq!(status["running"], 0);
}

/// Many requests at once: each answer reaches its own caller.
#[test]
fn many_requests_at_once_are_each_answered_to_the_one_that_asked() {
    let bench = Bench::new();
    let _daemon = bench.start(30.0);
    let client = std::sync::Arc::new(bench.connect());
    let askers: Vec<_> = (0..16)
        .map(|n| {
            let client = std::sync::Arc::clone(&client);
            std::thread::spawn(move || {
                let id = format!("sbx-nobody-{n}");
                let refused = client.call("sandbox.view", json!({"id": id})).unwrap();
                assert!(refused.error.unwrap().message.contains(&id));
            })
        })
        .collect();
    for asker in askers {
        asker.join().unwrap();
    }
}

#[test]
fn a_line_it_cannot_read_is_refused_and_hung_up_on() {
    use std::io::{BufRead, BufReader, Write};
    let bench = Bench::new();
    let _daemon = bench.start(30.0);
    drop(bench.connect()); // it is up

    #[cfg(unix)]
    let mut stream = std::os::unix::net::UnixStream::connect(&bench.socket).unwrap();
    #[cfg(windows)]
    let mut stream =
        beatbox_contract::wire::pipe::connect(&bench.socket, Duration::from_secs(5)).unwrap();
    writeln!(stream, "{{\"id\": \"r1\", \"action\": \"sandbox.list\"}}").unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.contains("\"refused\""), "{line}");
    assert!(line.contains("the first line is a hello"), "{line}");
    line.clear();
    assert_eq!(
        reader.read_line(&mut line).unwrap(),
        0,
        "it did not hang up"
    );
}

/// Mismatched contract: actions refused with a reason, exit still allowed.
#[test]
fn a_window_from_another_contract_is_told_why_and_may_still_ask_it_to_go() {
    let bench = Bench::new();
    let mut daemon = bench.start(30.0);
    let client = bench.connect_as(CONTRACT_VERSION + 1);
    assert_eq!(client.theirs().contract, CONTRACT_VERSION);
    let refused = client.call("sandbox.list", json!({})).unwrap();
    assert_eq!(refused.error.unwrap().code, "wrong-contract");
    assert_eq!(client.daemon("status").unwrap()["owner"], true);
    assert_eq!(client.daemon("exit").unwrap()["exiting"], true);
    assert!(
        daemon
            .ended_within(Duration::from_secs(5))
            .unwrap()
            .success()
    );
    assert!(!bench.socket.exists());
}

#[test]
fn with_nothing_needing_it_the_daemon_goes() {
    let bench = Bench::new();
    let mut daemon = bench.start(1.0);
    let client = bench.connect();
    // Not while a window is there.
    assert!(daemon.ended_within(Duration::from_secs(2)).is_none());
    drop(client);
    let status = daemon
        .ended_within(Duration::from_secs(5))
        .expect("it stayed with nothing to do");
    assert!(status.success());
    assert!(!bench.listening(), "it left its socket behind");
    assert!(bench.log().contains("nothing has needed this daemon"));
}

#[test]
fn a_second_daemon_on_the_same_socket_leaves_the_first_alone() {
    let bench = Bench::new();
    let _first = bench.start(30.0);
    let client = bench.connect();
    let mut second = bench.start(30.0);
    assert!(
        second
            .ended_within(Duration::from_secs(5))
            .unwrap()
            .success()
    );
    assert_eq!(client.daemon("status").unwrap()["owner"], true);
    drop(client);
    assert!(
        bench.connect().daemon("status").is_ok(),
        "the first stopped answering"
    );
}

/// Someone else holds the owner lock: the daemon exits instead of serving an empty runtime.
#[test]
fn a_daemon_that_cannot_own_the_machines_does_not_serve() {
    let bench = Bench::new();
    fs::create_dir_all(bench.home.join("state")).unwrap();
    let owner = fs::OpenOptions::new()
        .create(true)
        .append(true)
        // Windows' LockFileEx wants read or write access; append is neither.
        .read(true)
        .open(bench.home.join("state/.owner"))
        .unwrap();
    owner.try_lock().unwrap();
    let mut daemon = bench.start(30.0);
    let status = daemon.ended_within(Duration::from_secs(10)).unwrap();
    assert_eq!(status.code(), Some(3));
    assert!(!bench.listening());
    assert!(bench.log().contains("another BeatBox owns the machines"));
}

#[cfg(unix)]
#[test]
fn a_socket_a_dead_daemon_left_is_replaced() {
    let bench = Bench::new();
    drop(std::os::unix::net::UnixListener::bind(&bench.socket).unwrap()); // the file, and nobody
    let _daemon = bench.start(30.0);
    assert_eq!(bench.connect().daemon("status").unwrap()["owner"], true);
}

#[cfg(unix)]
#[test]
fn the_socket_is_this_users_alone() {
    use std::os::unix::fs::PermissionsExt;
    let bench = Bench::new();
    let fresh = bench.root().join("fresh/run/d.sock");
    let _daemon = bench.start_on(&fresh, 30.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fresh.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(fresh.parent().unwrap()), 0o700);
    assert_eq!(mode(&fresh), 0o600);

    // A folder others can read is refused, and left exactly as it was.
    let open = bench.root().join("open");
    fs::create_dir_all(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
    let mut refused = bench.start_on(&open.join("d.sock"), 30.0);
    assert_eq!(
        refused
            .ended_within(Duration::from_secs(10))
            .unwrap()
            .code(),
        Some(1)
    );
    assert_eq!(mode(&open), 0o755);
}

/// The pipe's DACL is protected with a single entry: this user's SID.
#[cfg(windows)]
#[test]
fn the_pipe_is_this_users_alone() {
    let sddl = beatbox_contract::wire::pipe::only_this_user().unwrap();
    assert!(sddl.starts_with("D:P(A;;GA;;;S-1-"), "{sddl}");
    assert_eq!(sddl.matches('(').count(), 1, "{sddl}");
    assert!(beatbox_contract::wire::pipe::process_is_ours(
        std::process::id()
    ));
}

fn have_qemu() -> bool {
    ["qemu-system-x86_64", "qemu-img"].iter().all(|program| {
        Command::new(program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

fn wait_for(events: &std::sync::mpsc::Receiver<(String, Value)>, name: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok((kind, payload)) = events.recv_timeout(Duration::from_millis(200))
            && kind == "sandbox"
            && payload["name"] == name
        {
            return payload;
        }
    }
    panic!("never heard {name}");
}

fn lifecycle(client: &Client, id: &str) -> Value {
    data(client.call("sandbox.view", json!({"id": id})).unwrap())["state"]["lifecycle"].clone()
}

/// The full handover on real QEMU: a running machine keeps the daemon up with
/// no window; the next window finds it; that daemon exits without stopping
/// it; the next one reattaches from the records and stops it.
#[test]
fn a_machine_left_running_outlives_its_window_and_its_daemon() {
    if !have_qemu() {
        eprintln!("skipped: QEMU is not installed on this host");
        return;
    }
    let bench = Bench::new();
    fs::create_dir_all(&bench.home).unwrap();
    let disk = bench.home.join("empty.qcow2");
    assert!(
        Command::new("qemu-img")
            .args(["create", "-f", "qcow2"])
            .arg(&disk)
            .arg("64M")
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    );

    let mut first = bench.start(1.0);
    let window = bench.connect();
    let events = window.events().unwrap();
    let made = data(
        window
            .call(
                "sandbox.create",
                json!({"type": "host", "name": "left-running", "cpu": 1, "memory": 256,
                       "settings": {"disk": disk.to_string_lossy()}, "uses": {}}),
            )
            .unwrap(),
    );
    let id = made["id"].as_str().unwrap().to_owned();
    data(
        window
            .call(
                "sandbox.transition",
                json!({"id": id, "transition": "start"}),
            )
            .unwrap(),
    );
    wait_for(&events, "sandbox.started");
    assert_eq!(window.daemon("status").unwrap()["running"], 1);

    // The window closes with "leave it running".
    drop(window);
    assert!(
        first.ended_within(Duration::from_secs(3)).is_none(),
        "the daemon went with a machine still running"
    );
    let window = bench.connect();
    assert_eq!(lifecycle(&window, &id), "running");

    // An upgrade: the old daemon goes, and leaves the machine as it is.
    assert_eq!(window.daemon("exit").unwrap()["running"], 1);
    assert!(
        first
            .ended_within(Duration::from_secs(5))
            .unwrap()
            .success()
    );
    drop(window);

    let mut second = bench.start(1.0);
    let window = bench.connect();
    assert_ne!(window.theirs().pid, Some(first.pid()));
    assert_eq!(lifecycle(&window, &id), "running");
    let refreshed = data(window.call("sandbox.refresh", json!({"id": id})).unwrap());
    assert_eq!(refreshed["state"]["lifecycle"], "running", "{refreshed}");

    // The window's "stop" on its way out.
    assert_eq!(
        window.daemon("stop-running").unwrap()["stubborn"],
        json!([])
    );
    assert_eq!(lifecycle(&window, &id), "stopped");
    data(
        window
            .call(
                "sandbox.transition",
                json!({"id": id, "transition": "destroy"}),
            )
            .unwrap(),
    );
    drop(window);
    assert!(
        second
            .ended_within(Duration::from_secs(5))
            .unwrap()
            .success()
    );
}
