//! The Windows guest tools, fetched once and put in front of the guest.
//!
//! A stock Windows has neither the agent nor the virtio-serial driver it
//! talks through, and BeatBox can't install them for the guest without the
//! agent. So virtio-win's installer is fetched once and plugged in as a
//! read-only USB drive (`media.insert`): one double-click inside.
//!
//! **Pinned and checked.** Fedora publishes no checksum for this `.exe`, so
//! the SHA-256 is from the file fetched over HTTPS on 2026-09-23. A
//! mismatch is deleted, never offered to a guest.
//!
//! Nothing is downloaded until somebody presses the button that says what
//! and how big.
//!
//! **Fetched by `curl`**, which every supported system has (Windows since 10
//! 1803), HTTPS only. The file is trusted on its hash, not its transport, so
//! linking a TLS stack for one file buys nothing (see `deny.toml`).

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use beatbox_contract::{ActionOutcomeView, GuestToolsView};
use beatbox_engine::{Data, Sandbox, reason};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::Api;
use crate::fault::ApiFault;
use crate::jobs::{Jobs, Said, Say};
use crate::sandbox::{outcome_view, reason_view};

/// What the drive is called inside the guest. FAT labels are 11 characters.
pub const LABEL: &str = "BEATBOX";

/// The download's job kind, so no screen mistakes it for an install.
pub const JOB: &str = "tools";

/// The capability that plugs it in.
pub const CAPABILITY: &str = "media.insert";

/// What is fetched, and what it has to be. A new version is an edit here,
/// never automatic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pinned {
    /// The virtio-win release.
    pub version: String,
    /// The installer's name, and the only file in its folder.
    pub filename: String,
    /// Where it is fetched from.
    pub url: String,
    /// What it hashes to.
    pub sha256: String,
    /// How big it is: shown on the button, and the denominator of progress.
    pub size: u64,
}

impl Default for Pinned {
    fn default() -> Self {
        let version = "0.1.302";
        let filename = "virtio-win-guest-tools.exe";
        Self {
            version: version.into(),
            filename: filename.into(),
            url: format!(
                "https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/\
                 archive-virtio/virtio-win-{version}-1/{filename}"
            ),
            sha256: "d8ae9ea1e943384ac195e012cd36f82c3161620e5fc9529be4e42e6f420e222d".into(),
            size: 31_642_483,
        }
    }
}

/// Opening a download: what the tests stand in for.
pub trait Fetch: Send + Sync {
    /// The bytes at `url`, as they arrive. An error at any point, even after
    /// the last byte, is the download failing.
    fn open(&self, url: &str, timeout: Duration) -> io::Result<Box<dyn Read + Send>>;
}

/// `curl`, for real.
#[derive(Debug, Clone, Copy, Default)]
pub struct Curl;

impl Fetch for Curl {
    fn open(&self, url: &str, timeout: Duration) -> io::Result<Box<dyn Read + Send>> {
        let seconds = timeout.as_secs().max(1).to_string();
        let mut child = Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                // A stall fails, a slow line doesn't: no overall timeout.
                "--connect-timeout",
                &seconds,
                "--speed-limit",
                "1",
                "--speed-time",
                &seconds,
                "--",
                url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        Ok(Box::new(CurlStream {
            child,
            stdout,
            finished: false,
        }))
    }
}

/// curl's output, which is only a download if curl says so at the end.
struct CurlStream {
    child: Child,
    stdout: ChildStdout,
    finished: bool,
}

impl Read for CurlStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.finished {
            return Ok(0);
        }
        let read = self.stdout.read(buffer)?;
        if read > 0 {
            return Ok(read);
        }
        self.finished = true;
        let status = self.child.wait()?;
        if status.success() {
            return Ok(0);
        }
        let mut said = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut said);
        }
        let said = said.trim();
        Err(io::Error::other(if said.is_empty() {
            format!("curl exited with {status}")
        } else {
            said.to_owned()
        }))
    }
}

impl Drop for CurlStream {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The guest tools: what they are, where they go, and how they are fetched.
#[derive(Clone)]
pub struct Tools {
    /// What is fetched.
    pub pinned: Pinned,
    /// How.
    pub fetch: Arc<dyn Fetch>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            pinned: Pinned::default(),
            fetch: Arc::new(Curl),
        }
    }
}

impl Api {
    /// One folder per version holding only the one file: the guest sees the
    /// whole folder.
    pub fn tools_folder(&self) -> PathBuf {
        self.paths
            .tools
            .join(format!("virtio-win-{}", self.tools.pinned.version))
    }

    /// Whether they are here.
    pub fn tools_ready(&self) -> bool {
        self.tools_folder()
            .join(&self.tools.pinned.filename)
            .is_file()
    }

    pub(crate) fn tools_status(&self) -> Result<Value, ApiFault> {
        let view = GuestToolsView {
            ready: self.tools_ready(),
            version: self.tools.pinned.version.clone(),
            bytes: i64::try_from(self.tools.pinned.size).unwrap_or(i64::MAX),
        };
        Ok(serde_json::to_value(view).expect("a view is JSON"))
    }

    /// Download them, as a job. Refused rather than repeated if they are here.
    pub(crate) fn tools_fetch(&self) -> Result<Value, ApiFault> {
        if self.tools_ready() {
            return Err(ApiFault::new(
                "already",
                "The guest tools are already here.",
            ));
        }
        let pinned = self.tools.pinned.clone();
        let fetch = Arc::clone(&self.tools.fetch);
        let folder = self.tools_folder();
        let job = run(&self.jobs, JOB, move |say| {
            download(say, &pinned, fetch.as_ref(), folder)
        })?;
        Ok(crate::jobs::wire(&job))
    }

    /// Plug them into one running machine. An outcome, never a fault, unless
    /// the sandbox isn't there.
    pub(crate) fn tools_insert(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let sandbox = self.sandbox(&payload["sandbox_id"])?;
        let view = if self.tools_ready() {
            outcome_view(&self.insert_tools(&sandbox)?)
        } else {
            ActionOutcomeView {
                capability: CAPABILITY.into(),
                ok: false,
                data: Value::Object(Map::new()),
                reason: Some(reason_view(&reason!("tools.not_here"))),
            }
        };
        Ok(serde_json::to_value(view).expect("a view is JSON"))
    }

    fn insert_tools(&self, sandbox: &Sandbox) -> Result<beatbox_engine::ActionOutcome, ApiFault> {
        insert(&self.tools_folder(), sandbox)
    }

    /// After a start: plug the tools in if they're here. Silent best effort;
    /// a machine that can't take them doesn't declare the capability.
    pub fn offer_tools(&self, sandbox: &Sandbox) {
        offer(&self.tools_folder(), &self.tools.pinned.filename, sandbox);
    }
}

fn insert(folder: &Path, sandbox: &Sandbox) -> Result<beatbox_engine::ActionOutcome, ApiFault> {
    let mut arguments = Data::new();
    arguments.insert(
        "folder".into(),
        Value::String(folder.to_string_lossy().into_owned()),
    );
    arguments.insert("label".into(), Value::String(LABEL.into()));
    sandbox
        .act(CAPABILITY, arguments)
        .map_err(|error| crate::sandbox::contract(&error))
}

/// [`Api::offer_tools`], for a caller that holds the folder rather than the api.
pub(crate) fn offer(folder: &Path, filename: &str, sandbox: &Sandbox) {
    if !folder.join(filename).is_file() || !sandbox.capabilities().supports(CAPABILITY) {
        return;
    }
    match insert(folder, sandbox) {
        Ok(outcome) if !outcome.ok => log::info!(
            "BeatBox: could not offer the guest tools to {}: {:?}",
            sandbox.id(),
            outcome.reason
        ),
        Ok(_) => {}
        Err(fault) => log::info!(
            "BeatBox: could not offer the guest tools to {}: {fault}",
            sandbox.id()
        ),
    }
}

/// `jobs.run`, with busy turned into a `busy` fault.
pub(crate) fn run(
    jobs: &Jobs,
    kind: &str,
    work: impl FnOnce(Say<'_>) -> (bool, Said) + Send + 'static,
) -> Result<beatbox_contract::Job, ApiFault> {
    jobs.run(kind, work)
        .map_err(|busy| ApiFault::new("busy", busy.0))
}

fn download(say: Say<'_>, pinned: &Pinned, fetch: &dyn Fetch, folder: PathBuf) -> (bool, Said) {
    let failed = |error: &io::Error| {
        (
            false,
            Said::Reason(reason!("tools.download_failed", detail = error)),
        )
    };
    if let Err(error) = fs::create_dir_all(&folder) {
        return failed(&error);
    }
    let parent = folder.parent().map(PathBuf::from).unwrap_or_default();
    let partial = parent.join(format!(".{}.part", pinned.filename));
    let mut digest = Sha256::new();
    let fetched = (|| -> io::Result<()> {
        let mut response = fetch.open(&pinned.url, Duration::from_secs(30))?;
        let mut out = fs::File::create(&partial)?;
        let mut chunk = vec![0u8; 1 << 16];
        let mut done: u64 = 0;
        loop {
            let read = response.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            io::Write::write_all(&mut out, &chunk[..read])?;
            digest.update(&chunk[..read]);
            done += read as u64;
            if done % (4 << 20) < read as u64 {
                say(&format!("{}%", done * 100 / pinned.size.max(1)));
            }
        }
        Ok(())
    })();
    if let Err(error) = fetched {
        let _ = fs::remove_file(&partial);
        return failed(&error);
    }
    let got: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if got != pinned.sha256 {
        // Not the file the pinned hash came from, so never offered.
        let _ = fs::remove_file(&partial);
        return (
            false,
            Said::Reason(reason!(
                "tools.wrong_file",
                expected = &pinned.sha256[..pinned.sha256.len().min(12)],
                got = &got[..12],
            )),
        );
    }
    if let Err(error) = fs::rename(&partial, folder.join(&pinned.filename)) {
        let _ = fs::remove_file(&partial);
        return failed(&error);
    }
    (true, Said::Text("ready".into()))
}
