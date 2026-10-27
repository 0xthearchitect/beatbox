//! Running a short program and reading what it said: `qemu-img`, `qemu
//! -display help`.
//!
//! A trait so a test can answer in its place. [`SystemRunner`] runs things.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// What a program left behind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// Its exit code; `-1` when it had none (a signal).
    pub code: i32,
    /// What it printed, as text.
    pub stdout: String,
    /// What it complained about, as text.
    pub stderr: String,
}

/// Something that runs `argv` and waits for it, up to `timeout`.
pub trait Runner: Send + Sync {
    /// Run it and wait. A program still going at the deadline is killed and
    /// is an error, not an answer.
    fn run(&self, argv: &[String], timeout: Option<Duration>) -> io::Result<Output>;
}

/// Runs things, for real.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, argv: &[String], timeout: Option<Duration>) -> io::Result<Output> {
        let Some((program, arguments)) = argv.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nothing to run",
            ));
        };
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let out = thread::spawn(move || {
            let mut text = Vec::new();
            let _ = stdout.read_to_end(&mut text);
            text
        });
        let err = thread::spawn(move || {
            let mut text = Vec::new();
            let _ = stderr.read_to_end(&mut text);
            text
        });
        let deadline = timeout.map(|t| Instant::now() + t);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
            }
            thread::sleep(Duration::from_millis(10));
        };
        let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
        Ok(Output {
            code: status.code().unwrap_or(-1),
            stdout: text(out.join().unwrap_or_default()),
            stderr: text(err.join().unwrap_or_default()),
        })
    }
}
