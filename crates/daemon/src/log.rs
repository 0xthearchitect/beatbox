//! `~/.beatbox/beatboxd.log`. Rotates to `.old` at [`LIMIT`], so it never
//! takes more than twice that on disk.
//!
//! `BEATBOX_LOG` sets how much is said: `error`, `warn`, `info` (the
//! default), `debug` or `trace`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use log::{LevelFilter, Log, Metadata, Record};

/// Rotate past this many bytes.
pub const LIMIT: u64 = 1 << 20;

struct Open {
    file: File,
    written: u64,
}

/// Where the lines go.
enum Sink {
    File {
        path: PathBuf,
        open: Mutex<Option<Open>>,
    },
    Stderr,
}

struct Logger {
    sink: Sink,
    level: LevelFilter,
}

fn level() -> LevelFilter {
    match std::env::var("BEATBOX_LOG").as_deref() {
        Ok("error") => LevelFilter::Error,
        Ok("warn") => LevelFilter::Warn,
        Ok("debug") => LevelFilter::Debug,
        Ok("trace") => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}

fn open(path: &Path) -> Option<Open> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    let written = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    Some(Open { file, written })
}

fn old(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            beatbox_api::now(),
            record.level(),
            record.target(),
            record.args()
        );
        match &self.sink {
            Sink::Stderr => {
                let _ = std::io::stderr().write_all(line.as_bytes());
            }
            Sink::File { path, open: held } => {
                let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
                let length = line.len() as u64;
                if held
                    .as_ref()
                    .is_some_and(|open| open.written > 0 && open.written + length > LIMIT)
                {
                    *held = None;
                    let _ = fs::rename(path, old(path));
                }
                if held.is_none() {
                    *held = open(path);
                }
                if let Some(open) = held.as_mut()
                    && open.file.write_all(line.as_bytes()).is_ok()
                {
                    open.written += length;
                }
            }
        }
    }

    fn flush(&self) {
        if let Sink::File { open: held, .. } = &self.sink
            && let Some(open) = held.lock().unwrap_or_else(PoisonError::into_inner).as_mut()
        {
            let _ = open.file.flush();
        }
    }
}

/// Log to `path`, or to stderr when it is `-`.
pub fn start(path: &Path) {
    let sink = if path == Path::new("-") {
        Sink::Stderr
    } else {
        Sink::File {
            path: path.to_path_buf(),
            open: Mutex::new(None),
        }
    };
    let level = level();
    // Leaked on purpose: lives as long as the process, and set_boxed_logger needs log's std feature.
    let logger: &'static Logger = Box::leak(Box::new(Logger { sink, level }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
    // Detached daemons have no stderr; log panics too, or they vanish silently.
    let said_there = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("{info}");
        log::logger().flush();
        said_there(info);
    }));
}
