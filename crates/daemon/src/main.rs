//! `beatboxd`, the process that owns the sandboxes.
//!
//! - Started by a window when nothing answers; never at login.
//! - One per user: the socket lock, then `~/.beatbox/state/.owner`. Can't
//!   get the second? Exit 3 rather than run a runtime that restores nothing.
//! - Exits after `--idle` seconds (30) with no window, nothing running or
//!   frozen, no install or download.
//! - Upgrades keep guests: exiting never stops a machine; the next daemon
//!   reattaches from the records.
//! - Logs to `~/.beatbox/beatboxd.log`, size-capped (log.rs).
//!
//! ```text
//! beatboxd [--socket PATH] [--idle SECONDS] [--log PATH|-]
//! ```
//!
//! Exit codes: 0 done/asked/another daemon already up; 2 bad arguments;
//! 3 another BeatBox owns the machines; 1 anything else.

mod log;
#[cfg(unix)]
mod place;
mod serve;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// Package version plus `BEATBOX_BUILD` from packaging, if set at compile time.
fn build() -> String {
    format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("BEATBOX_BUILD").unwrap_or("dev")
    )
}

/// Default idle time before exiting.
const IDLE: Duration = Duration::from_secs(30);

struct Options {
    socket: Option<PathBuf>,
    idle: Duration,
    log: Option<PathBuf>,
}

const USAGE: &str = "usage: beatboxd [--socket PATH] [--idle SECONDS] [--log PATH|-] | --version";

fn options() -> Result<Option<Options>, String> {
    let mut options = Options {
        socket: None,
        idle: IDLE,
        log: None,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| format!("{argument} needs a value"))
        };
        match argument.as_str() {
            "--socket" => options.socket = Some(PathBuf::from(value()?)),
            "--idle" => {
                let seconds: f64 = value()?
                    .parse()
                    .map_err(|_| "--idle is a number of seconds".to_owned())?;
                options.idle = Duration::from_secs_f64(seconds.max(0.0));
            }
            "--log" => options.log = Some(PathBuf::from(value()?)),
            "--version" => {
                println!("beatboxd {}", build());
                return Ok(None);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            other => return Err(format!("beatboxd does not take {other}")),
        }
    }
    Ok(Some(options))
}

fn main() -> ExitCode {
    // `pkexec` starts this mode alone when an ordinary user cannot read a
    // physical disk. Do not start the daemon, load records, or open a socket.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--beatdrop-read") {
        if args.len() != 3 {
            return ExitCode::from(2);
        }
        return match beatbox_api::beatdrop::read_device(&args[2]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("beatdrop: {error}");
                ExitCode::from(1)
            }
        };
    }
    // Same for an in-use disk: copied as root in its own mount namespace.
    #[cfg(unix)]
    match args.get(1).map(String::as_str) {
        Some("--beatdrop-live") if args.len() >= 5 => {
            return match beatbox_api::beatdrop::live::isolated(&args[2..]) {
                Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
                Err(error) => {
                    eprintln!("beatdrop: {error}");
                    ExitCode::from(1)
                }
            };
        }
        Some("--beatdrop-live-ns") if args.len() >= 5 => {
            return match beatbox_api::beatdrop::live::capture(
                &args[2],
                &args[3],
                &args[4],
                &args[5..],
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("beatdrop: {error}");
                    ExitCode::from(1)
                }
            };
        }
        Some("--beatdrop-live" | "--beatdrop-live-ns") => return ExitCode::from(2),
        _ => {}
    }
    let options = match options() {
        Ok(Some(options)) => options,
        Ok(None) => return ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("{problem}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    run(options)
}

#[cfg(unix)]
fn run(options: Options) -> ExitCode {
    use std::sync::Arc;

    use beatbox_api::{Api, ApiParts, Paths};

    let paths = Paths::here();
    let socket = options
        .socket
        .unwrap_or_else(beatbox_contract::wire::socket_path);
    log::start(
        &options
            .log
            .unwrap_or_else(|| paths.root.join("beatboxd.log")),
    );
    ::log::info!(
        "beatboxd {} starting as process {}, on {}",
        build(),
        std::process::id(),
        socket.display()
    );

    if let Err(error) = place::prepare(&socket) {
        ::log::error!("cannot listen there: {error}");
        eprintln!("beatboxd: {error}");
        return ExitCode::from(1);
    }
    let _only_one = match place::claim(&socket) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            ::log::info!("another daemon is already on this socket");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            ::log::error!("cannot take the socket's lock: {error}");
            return ExitCode::from(1);
        }
    };
    let listener = match place::listen(&socket) {
        Ok(listener) => listener,
        Err(error) => {
            ::log::error!("cannot listen on {}: {error}", socket.display());
            eprintln!("beatboxd: {error}");
            return ExitCode::from(1);
        }
    };

    // Listen before restoring machines, or a window finds nobody and starts a second daemon.
    let api = Arc::new(Api::open(ApiParts {
        paths,
        ..ApiParts::default()
    }));
    if !api.owns() {
        let _ = std::fs::remove_file(&socket);
        ::log::error!(
            "another BeatBox owns the machines in {}; not serving",
            api.paths().state.display()
        );
        eprintln!("beatboxd: another BeatBox owns the machines");
        return ExitCode::from(3);
    }
    ::log::info!(
        "serving, with {} sandbox(es) known and {} running",
        api.runtime().sandboxes().len(),
        api.running().len()
    );

    let server = serve::Server::new(api, build(), socket, options.idle);
    server.watch_idle();
    server.accept(
        || listener.accept().map(|(stream, _)| stream),
        place::same_user,
    );
    ExitCode::from(1)
}

/// Windows: a named pipe only this user can open (`wire::pipe`). "One
/// daemon" rests on the pipe's first instance failing if the name exists.
#[cfg(windows)]
fn run(options: Options) -> ExitCode {
    use std::sync::Arc;

    use beatbox_api::{Api, ApiParts, Paths};
    use beatbox_contract::wire::{self, pipe};

    let paths = Paths::here();
    let name = options
        .socket
        .map_or_else(wire::socket_path, |given| wire::pipe_name(&given));
    log::start(
        &options
            .log
            .unwrap_or_else(|| paths.root.join("beatboxd.log")),
    );
    ::log::info!(
        "beatboxd {} starting as process {}, on {}",
        build(),
        std::process::id(),
        name.display()
    );
    let listener = match pipe::Listener::bind(&name) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            ::log::info!("another daemon is already on this pipe");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            ::log::error!("cannot make the pipe {}: {error}", name.display());
            eprintln!("beatboxd: {error}");
            return ExitCode::from(1);
        }
    };
    let api = Arc::new(Api::open(ApiParts {
        paths,
        ..ApiParts::default()
    }));
    if !api.owns() {
        ::log::error!(
            "another BeatBox owns the machines in {}; not serving",
            api.paths().state.display()
        );
        eprintln!("beatboxd: another BeatBox owns the machines");
        return ExitCode::from(3);
    }
    ::log::info!(
        "serving, with {} sandbox(es) known and {} running",
        api.runtime().sandboxes().len(),
        api.running().len()
    );
    let server = serve::Server::new(api, build(), name, options.idle);
    server.watch_idle();
    server.accept(|| listener.accept(), pipe::PipeStream::peer_is_ours);
    ExitCode::from(1)
}
