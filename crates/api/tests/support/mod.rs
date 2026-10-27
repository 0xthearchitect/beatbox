//! What every facade suite wants: a room, an `Api` pointed into it, and fakes.

#![allow(dead_code)]

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use beatbox_api::disks::Guest;
use beatbox_api::guest_tools::{Fetch, Pinned, Tools};
use beatbox_api::setup::Host;
use beatbox_api::system::KvmFacts;
use beatbox_api::{Api, ApiParts, Installer, Launch, Paths, Setup};
use beatbox_contract::{Envelope, Job};
use beatbox_engine::{MockSandbox, RuntimeParts};
use beatbox_isolation::machine::{Display, ISO_MAGIC, ISO_MAGIC_OFFSET, Qemu};
use beatbox_isolation::runner::{Output, Runner};
use serde_json::Value;

/// A directory for one test to scribble in, removed when it is dropped.
pub struct Room(PathBuf);

impl Room {
    pub fn new(prefix: &str) -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "beatbox-api-tests-{}-{prefix}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(beatbox_isolation::host_os::canonical(&path).unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    pub fn text(&self, relative: &str) -> String {
        self.join(relative).to_string_lossy().into_owned()
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A file that sniffs as a disc. `is_optical` reads one magic string.
pub fn fake_disc(path: &Path) -> String {
    let mut data = vec![0u8; ISO_MAGIC_OFFSET as usize + ISO_MAGIC.len() + 4];
    data[ISO_MAGIC_OFFSET as usize..ISO_MAGIC_OFFSET as usize + ISO_MAGIC.len()]
        .copy_from_slice(ISO_MAGIC);
    std::fs::write(path, data).unwrap();
    path.to_string_lossy().into_owned()
}

/// A QEMU that is there, with nothing to accelerate it.
pub fn qemu() -> Qemu {
    Qemu {
        system: "qemu-system-x86_64".into(),
        image: "qemu-img".into(),
        system_found: true,
        image_found: true,
        kvm: false,
        whpx: false,
        cpu: None,
    }
}

/// `qemu-img`, as far as an install asks it anything: `create` makes the
/// file, empty, and every argv is kept.
#[derive(Default)]
pub struct FakeImage {
    pub argvs: Mutex<Vec<Vec<String>>>,
}

impl Runner for FakeImage {
    fn run(&self, argv: &[String], _timeout: Option<Duration>) -> io::Result<Output> {
        self.argvs.lock().unwrap().push(argv.to_vec());
        if argv.get(1).map(String::as_str) == Some("create") {
            std::fs::write(&argv[4], b"")?;
        }
        Ok(Output::default())
    }
}

/// What a fix is run by in a test: every argv kept, and one answer for all.
#[derive(Default)]
pub struct Answers {
    pub argvs: Mutex<Vec<Vec<String>>>,
    pub answer: Output,
}

impl Runner for Answers {
    fn run(&self, argv: &[String], _timeout: Option<Duration>) -> io::Result<Output> {
        self.argvs.lock().unwrap().push(argv.to_vec());
        Ok(self.answer.clone())
    }
}

/// An Arch Linux with nothing of QEMU's, /dev/kvm this user cannot open, and
/// pkexec: everything missing, and everything fixable from here.
pub fn bare_arch() -> Host {
    Host {
        windows: false,
        qemu: false,
        window: false,
        kvm: KvmFacts {
            usable: false,
            windows: false,
            whpx: false,
            linux: true,
            device: true,
        },
        in_kvm_group: false,
        os_release: "NAME=\"Arch Linux\"\nID=arch\n".into(),
        pkexec: true,
        winget: false,
        user: "alice".into(),
    }
}

/// `host`, and fixes answered by `runner` rather than run.
pub fn setup(host: Host, runner: Arc<Answers>) -> Setup {
    Setup {
        host: Arc::new(move || host.clone()),
        runner,
    }
}

/// A guest that does not end itself -- which is every Windows. Running until
/// something stops it, the way a machine at a fresh desktop is.
#[derive(Clone, Default)]
pub struct Stayed {
    pub stopped: Arc<AtomicBool>,
    pub terminated: Arc<AtomicBool>,
}

impl Stayed {
    /// As if it were shut down from inside.
    pub fn end(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

impl Guest for Stayed {
    fn running(&mut self) -> bool {
        !self.stopped.load(Ordering::SeqCst)
    }

    fn stop(&mut self, _timeout: Duration) {
        self.terminated.store(true, Ordering::SeqCst);
        self.stopped.store(true, Ordering::SeqCst);
    }
}

/// A guest that has already ended: a Linux install that powered off.
pub struct Finished;

impl Guest for Finished {
    fn running(&mut self) -> bool {
        false
    }

    fn stop(&mut self, _timeout: Duration) {}
}

/// A launcher that records the argv and starts nothing.
#[derive(Default)]
pub struct Ran {
    pub commands: Mutex<Vec<Vec<String>>>,
    pub guest: Mutex<Option<Stayed>>,
}

impl Ran {
    pub fn with(guest: Stayed) -> Self {
        Self {
            commands: Mutex::default(),
            guest: Mutex::new(Some(guest)),
        }
    }
}

impl Launch for Ran {
    fn launch(&self, argv: &[String]) -> io::Result<Box<dyn Guest>> {
        self.commands.lock().unwrap().push(argv.to_vec());
        Ok(match self.guest.lock().unwrap().clone() {
            Some(guest) => Box::new(guest),
            None => Box::new(Finished),
        })
    }
}

/// Serves bytes from memory.
pub struct Serving(pub Vec<u8>);

impl Fetch for Serving {
    fn open(&self, _url: &str, _timeout: Duration) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(io::Cursor::new(self.0.clone())))
    }
}

/// No network at all, for every test not about the download.
pub struct Failing;

impl Fetch for Failing {
    fn open(&self, _url: &str, _timeout: Duration) -> io::Result<Box<dyn Read + Send>> {
        Err(io::Error::other("network is unreachable"))
    }
}

/// An install's view of this computer: QEMU there, a window possible,
/// `qemu-img` faked, nothing ever spawned.
pub fn installer(launch: Arc<dyn Launch>, display: bool) -> Installer {
    Installer {
        qemu: Arc::new(qemu),
        display: Arc::new(move |_| Display {
            available: display,
            install_hint: "sudo pacman -S qemu-ui-gtk".into(),
        }),
        runner: Arc::new(FakeImage::default()),
        launch,
        uuid: Arc::new(beatbox_isolation::uuid4),
        grace: Duration::from_millis(500),
        listen_every: Duration::from_secs(5),
    }
}

/// Everything under `room`, `mock` the one type, nothing reachable.
pub fn parts(room: &Path) -> ApiParts {
    ApiParts {
        paths: Paths::under(room),
        types: vec![MockSandbox::spec()],
        runtime: RuntimeParts::default(),
        workspace: None,
        installer: installer(Arc::new(Ran::default()), true),
        tools: Tools {
            pinned: Pinned::default(),
            fetch: Arc::new(Failing),
        },
        setup: setup(bare_arch(), Arc::default()),
    }
}

pub fn open(room: &Path) -> Api {
    Api::open(parts(room))
}

/// Dispatch, the way the page does.
pub fn call(api: &Api, action: &str, payload: Value) -> Envelope {
    api.dispatch(action, &payload)
}

/// The fault's code, or a panic naming what came back instead.
pub fn code(envelope: &Envelope) -> String {
    envelope
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("it answered: {:?}", envelope.data))
        .code
        .clone()
}

/// The fault's reason's code.
pub fn reason_code(envelope: &Envelope) -> String {
    envelope
        .error
        .as_ref()
        .and_then(|error| error.reason.as_ref())
        .map(|reason| reason.code.clone())
        .unwrap_or_default()
}

/// The data of an answer, or a panic naming the fault.
pub fn data(envelope: Envelope) -> Value {
    assert!(envelope.ok, "{:?}", envelope.error);
    envelope.data.unwrap_or(Value::Null)
}

pub fn job(envelope: Envelope) -> Job {
    serde_json::from_value(data(envelope)).unwrap()
}

/// Wait for a job to stop running, and hand back where it landed.
pub fn settle(api: &Api, job_id: &str, within: Duration) -> Job {
    let deadline = Instant::now() + within;
    loop {
        let job = api.jobs().get(job_id).unwrap();
        if job.state != beatbox_api::jobs::RUNNING || Instant::now() > deadline {
            return job;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait for something to become true, or give up after `within`.
pub fn eventually(within: Duration, mut true_yet: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if true_yet() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true_yet()
}

// --- what was logged ----------------------------------------------------------

struct Capture;

static LINES: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

impl log::Log for Capture {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        LINES
            .lock()
            .unwrap()
            .push((record.level(), record.args().to_string()));
    }

    fn flush(&self) {}
}

/// Start keeping what is logged.
pub fn capture_logs() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        log::set_logger(&Capture).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
    });
}

/// Whether anything at `level` or worse mentioned `needle`. Suites run in
/// parallel, so what one test logged is found by something only it names.
pub fn logged(level: log::Level, needle: &str) -> bool {
    LINES
        .lock()
        .unwrap()
        .iter()
        .any(|(at, line)| *at <= level && line.contains(needle))
}
