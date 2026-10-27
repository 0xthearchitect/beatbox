//! The window's one connection to `beatboxd`.
//!
//! If nothing answers, the window starts `beatboxd` detached, in its own
//! process group so a terminal closing on the window doesn't take the
//! daemon and machines with it, then connects again. It inherits this
//! environment, so a machine's window opens on the same display.
//!
//! A connection that drops (the daemon was upgraded, or died) is made again
//! on the next request, starting the daemon if it is gone. A request is sent
//! again only if it never left: whether a create or a start that was cut off
//! happened is not something to find out by doing it twice.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use beatbox_contract::wire::{Client, Hello};
use beatbox_contract::{ApiError, Envelope, ReasonView, door};
use serde_json::{Value, json};

/// How long a started daemon gets to begin answering: it brings back every
/// machine the records name before it says hello.
pub const START_WITHIN: Duration = Duration::from_secs(30);

/// What this side calls itself in `hello`.
pub fn build() -> String {
    format!("{}+shell", env!("CARGO_PKG_VERSION"))
}

/// Where the daemon's binary is: `BEATBOX_DAEMON_BIN`; for a debug window,
/// the checkout's workspace build; beside this window's own, which is where
/// a package puts it; for a release window, the checkout's release build;
/// then the `PATH`.
///
/// Debug looks at the workspace build first on purpose: shell and daemon are
/// separate Cargo workspaces, so a daemon copied beside the shell goes stale
/// and the fresh build must win. A release from a checkout has nothing
/// beside it (only packaging puts it there); a package never has this path.
pub fn daemon_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "beatboxd.exe"
    } else {
        "beatboxd"
    };
    if let Some(explicit) = std::env::var_os("BEATBOX_DAEMON_BIN").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(explicit));
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target");
    if cfg!(debug_assertions) {
        for build in ["debug", "release"] {
            let candidate = workspace.join(build).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(beside) = exe.parent().map(|folder| folder.join(name))
        && beside.is_file()
    {
        return Some(beside);
    }
    let built = workspace.join("release").join(name);
    if built.is_file() {
        return Some(built);
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|folder| folder.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// A daemon that could not be reached, as the page is told: named, so that it
/// says so in the reader's language.
pub fn unreachable(detail: &str) -> Envelope {
    let mut values = serde_json::Map::new();
    values.insert("detail".into(), json!(detail));
    door::refused(ApiError {
        code: "no-daemon".into(),
        message: detail.to_owned(),
        reason: Some(ReasonView {
            code: "daemon.unreachable".into(),
            values: Value::Object(values),
        }),
    })
}

type Listener = Arc<dyn Fn(String, Value) + Send + Sync>;

/// The connection, made on first use and again whenever it has gone.
pub struct Link {
    socket: PathBuf,
    start: bool,
    client: Mutex<Option<Arc<Client>>>,
    listener: Arc<Mutex<Option<Listener>>>,
}

impl Link {
    /// A link to the daemon at `socket`; `start` says whether it may start
    /// one when nothing answers.
    pub fn new(socket: PathBuf, start: bool) -> Self {
        Self {
            socket,
            start,
            client: Mutex::new(None),
            listener: Arc::default(),
        }
    }

    /// Where every event the daemon says goes, from now on.
    pub fn listen(&self, listener: impl Fn(String, Value) + Send + Sync + 'static) {
        *self.listener.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(listener));
    }

    fn connect(&self) -> io::Result<Client> {
        Client::connect(&self.socket, &Hello::from(&build()), START_WITHIN)
    }

    fn spawn(&self) -> io::Result<std::process::Child> {
        let program = daemon_binary().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "beatboxd is not installed beside BeatBox",
            )
        })?;
        log::info!("BeatBox: starting {}", program.display());
        let mut command = Command::new(program);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }
        command.spawn()
    }

    /// The live connection, or a new one -- starting the daemon if nothing
    /// answers and this link may.
    fn client(&self) -> io::Result<Arc<Client>> {
        let mut held = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(client) = held.as_ref().filter(|client| client.alive()) {
            return Ok(Arc::clone(client));
        }
        let client = match self.connect() {
            Ok(client) => client,
            Err(error) if !self.start => return Err(error),
            Err(_) => {
                let mut started = self.spawn()?;
                let deadline = Instant::now() + START_WITHIN;
                loop {
                    match self.connect() {
                        Ok(client) => break client,
                        Err(error) => {
                            match started.try_wait() {
                                Ok(Some(status)) if status.code() == Some(3) => {
                                    return Err(io::Error::other(
                                        "another BeatBox owns the machines",
                                    ));
                                }
                                Ok(Some(status)) if !status.success() => {
                                    return Err(io::Error::other(format!(
                                        "beatboxd did not start ({status})"
                                    )));
                                }
                                _ => {}
                            }
                            if Instant::now() > deadline {
                                return Err(error);
                            }
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            }
        };
        let client = Arc::new(client);
        if let Some(events) = client.events() {
            let listener = Arc::clone(&self.listener);
            thread::Builder::new()
                .name("beatbox-events".into())
                .spawn(move || {
                    for (name, payload) in events {
                        let told = listener
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        if let Some(told) = told {
                            told(name, payload);
                        }
                    }
                })?;
        }
        *held = Some(Arc::clone(&client));
        Ok(client)
    }

    fn forget(&self, client: &Arc<Client>) {
        let mut held = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if held.as_ref().is_some_and(|kept| Arc::ptr_eq(kept, client)) {
            *held = None;
        }
    }

    /// One action, answered in the contract's envelope. Never fails: a
    /// daemon that cannot be reached is an envelope that says so.
    pub fn call(&self, action: &str, payload: &Value) -> Envelope {
        for _attempt in 0..2 {
            let client = match self.client() {
                Ok(client) => client,
                Err(error) => return unreachable(&error.to_string()),
            };
            match client.call(action, payload.clone()) {
                Ok(envelope) => return envelope,
                Err(error) if error.kind() == io::ErrorKind::NotConnected => {
                    self.forget(&client);
                }
                Err(_) => {
                    self.forget(&client);
                    return unreachable("beatboxd went before it answered");
                }
            }
        }
        unreachable("beatboxd could not be reached")
    }

    /// Ask the process itself: `status`, `stop-running`, `exit`.
    pub fn daemon(&self, what: &str) -> io::Result<Value> {
        let client = self.client()?;
        let answer = client.daemon(what);
        if answer.is_err() || what == "exit" {
            self.forget(&client);
        }
        answer
    }

    /// How the daemon is, if one is there -- never starting one to ask.
    pub fn status(&self) -> Option<Value> {
        let peek = Link::new(self.socket.clone(), false);
        let held = self
            .client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match held.filter(|client| client.alive()) {
            Some(client) => client.daemon("status").ok(),
            None => peek.daemon("status").ok(),
        }
    }
}
