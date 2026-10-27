//! The `process` runtime: a program inside the Host the Server uses.
//!
//! Everything goes through `peers.act("host", …)`, so through the Host's own
//! gates and onto its bus.
//!
//! **Nothing a definition or a caller says is ever a line a shell reads.**
//! The scripts below are constants of this crate; what changes from one
//! service to the next arrives as positional parameters (`"$@"`) to
//! `/bin/sh -c`, and an argv element is never parsed again. A configuration
//! value of `; reboot` is a strange argument, not a second command.

use beatbox_engine::{Data, Peers, Reason, data, reason};
use serde_json::Value;

/// Where a service's output goes, inside the guest.
pub const LOG_DIR: &str = "/var/log/beatbox";

/// Check the executable in the same directory and PATH used to start it.
/// Definition values remain literal positional parameters.
pub const PROGRAM_CHECK: &str = r#"cd "$1" || exit 2
program=$(command -v -- "$2" 2>/dev/null) || exit 127
[ -f "$program" ] && [ -x "$program" ] || exit 127"#;

/// Say each of `"$@"` this guest has: a name found on the PATH, or an
/// absolute path that exists. Read, never run.
pub const HAS: &str = r#"for p; do
case "$p" in
/*) [ -e "$p" ] && printf '%s\n' "$p" ;;
*) command -v -- "$p" >/dev/null 2>&1 && printf '%s\n' "$p" ;;
esac
done
exit 0"#;

/// Start `"$@"` detached from the agent, its output to `$1/$2.log`, in
/// directory `$3`, and say its pid. `setsid` in the background of a
/// non-interactive shell is not a group leader, so it execs: the pid echoed
/// is the service's own.
pub const START: &str = r#"mkdir -p "$1" || exit 1
log="$1/$2.log"
cd "$3" || exit 1
shift 3
setsid "$@" >"$log" 2>&1 </dev/null &
echo $!"#;

/// Exit 0 when pid `$1` is alive *and* is still `$2` -- a pid handed to
/// something else since is not the service.
pub const ALIVE: &str = r#"kill -0 "$1" 2>/dev/null || exit 1
first=$(tr '\0' '\n' < "/proc/$1/cmdline" | head -n 1)
[ "$first" = "$2" ]"#;

/// Ask pid `$1` to end, give it five seconds, then insist.
pub const STOP: &str = r#"kill -TERM "$1" 2>/dev/null || exit 0
i=0
while kill -0 "$1" 2>/dev/null && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i+1)); done
kill -KILL "$1" 2>/dev/null
exit 0"#;

/// Exit 0 when something on this guest accepts a TCP connection on port
/// `$1`. Run by `bash` under `timeout`: `/dev/tcp` is bash's.
pub const TCP_CHECK: &str = r#"exec 2>/dev/null 3<>"/dev/tcp/127.0.0.1/$1""#;

/// Exit 0 when `GET $2` on port `$1` answers a 2xx or 3xx status.
pub const HTTP_CHECK: &str = r#"exec 2>/dev/null 3<>"/dev/tcp/127.0.0.1/$1" || exit 1
printf 'GET %s HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n' "$2" >&3
IFS= read -r status <&3
status=${status%$'\r'}
code=${status#* }
code=${code%% *}
case "$code" in [23][0-9][0-9]) exit 0 ;; esac
exit 1"#;

/// A self-signed certificate for domain `$2` and every name under it, in
/// folder `$1`: made once and kept, so a browser told to trust it once is
/// not asked again. 127 when there is no openssl.
pub const CERTIFICATE: &str = r#"[ -s "$1/certificate.pem" ] && [ -s "$1/key.pem" ] && exit 0
command -v openssl >/dev/null 2>&1 || exit 127
mkdir -p -m 700 "$1" || exit 1
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 825 \
  -keyout "$1/key.pem" -out "$1/certificate.pem" -subj "/CN=$2" \
  -addext "subjectAltName=DNS:$2,DNS:*.$2" >/dev/null 2>&1 || exit 1
chmod 600 "$1/key.pem""#;

/// How long one check may take, inside the guest.
pub const CHECK_SECONDS: &str = "3";

/// The most log lines one `service.logs` returns.
pub const MOST_LINES: u64 = 1000;

/// One action on the Host, its data on success, its reason otherwise.
fn host(peers: &Peers, capability: &str, arguments: Data) -> Result<Data, Reason> {
    let outcome = peers
        .act("host", capability, arguments)
        .expect("the Server's capabilities are valid names");
    if outcome.ok {
        Ok(outcome.data)
    } else {
        let why = outcome
            .reason
            .unwrap_or_else(|| reason!("engine.unsupported", capability = capability));
        // This refusal came from the Host, not from the Server itself; and a
        // Host deleted under it is the Server's to explain, with what to do.
        Err(match why.code.as_str() {
            "engine.not_running" => reason!("server.host_unavailable"),
            "engine.peer_gone" => reason!("server.host_gone"),
            _ => why,
        })
    }
}

/// `argv`, run inside, with whatever it answers -- `exit_code`, `stdout`.
/// `secrets` are the service's secret values: the Host runs them but never
/// says them in `process.created`.
pub fn execute(peers: &Peers, argv: &[String], secrets: &[String]) -> Result<Data, Reason> {
    let (program, arguments) = argv
        .split_first()
        .expect("a definition's argv is never empty");
    let mut asked = data!({"command": program, "arguments": arguments});
    if !secrets.is_empty() {
        asked.insert("conceal".into(), secrets.into());
    }
    host(peers, "process.execute", asked)
}

fn sh(peers: &Peers, script: &str, parameters: &[&str]) -> Result<Data, Reason> {
    sh_concealing(peers, script, parameters, &[])
}

fn sh_concealing(
    peers: &Peers,
    script: &str,
    parameters: &[&str],
    secrets: &[String],
) -> Result<Data, Reason> {
    let mut argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
        "sh".to_owned(),
    ];
    argv.extend(parameters.iter().map(|p| (*p).to_owned()));
    execute(peers, &argv, secrets)
}

fn exit_code(data: &Data) -> i64 {
    data.get("exit_code").and_then(Value::as_i64).unwrap_or(-1)
}

/// Refuse a missing executable before spawning a detached process. A bad
/// working directory is left to the normal start error.
pub fn require_program(
    peers: &Peers,
    service: &str,
    directory: &str,
    program: &str,
) -> Result<(), Reason> {
    if exit_code(&sh(peers, PROGRAM_CHECK, &[directory, program])?) == 127 {
        return Err(reason!(
            "server.program_missing",
            service = service,
            program = program
        ));
    }
    Ok(())
}

/// Whether a bash `script` with `parameters`, given [`CHECK_SECONDS`],
/// exits 0.
fn check(peers: &Peers, script: &str, parameters: &[&str]) -> Result<bool, Reason> {
    let mut argv: Vec<String> = ["timeout", CHECK_SECONDS, "bash", "-c", script, "bash"]
        .map(str::to_owned)
        .to_vec();
    argv.extend(parameters.iter().map(|p| (*p).to_owned()));
    Ok(exit_code(&execute(peers, &argv, &[])?) == 0)
}

/// Something accepts a connection on `port`.
pub fn tcp_check(peers: &Peers, port: u16) -> Result<bool, Reason> {
    check(peers, TCP_CHECK, &[&port.to_string()])
}

/// `GET path` on `port` answers 2xx or 3xx.
pub fn http_check(peers: &Peers, port: u16, path: &str) -> Result<bool, Reason> {
    check(peers, HTTP_CHECK, &[&port.to_string(), path])
}

/// `argv` exits 0.
pub fn command_check(peers: &Peers, argv: &[String], secrets: &[String]) -> Result<bool, Reason> {
    let mut timed = vec!["timeout".to_owned(), CHECK_SECONDS.to_owned()];
    timed.extend(argv.iter().cloned());
    Ok(exit_code(&execute(peers, &timed, secrets)?) == 0)
}

/// A definition's prerequisite check, with an optional structured refusal.
pub fn prepare(
    peers: &Peers,
    service: &str,
    argv: &[String],
    secrets: &[String],
) -> Result<(), Reason> {
    let mut timed = vec!["timeout".to_owned(), CHECK_SECONDS.to_owned()];
    timed.extend(argv.iter().cloned());
    let answer = execute(peers, &timed, secrets)?;
    if exit_code(&answer) == 0 {
        return Ok(());
    }
    let stdout = answer
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Ok(mut why) = serde_json::from_str::<Reason>(stdout.trim()) {
        if why.values.contains_key("service") {
            why.values.insert("service".into(), service.to_owned());
        }
        return Err(why);
    }
    Err(reason!("server.prepare_failed", service = service))
}

/// The Server's shared certificate for `domain` in `folder`, made if it is
/// not there yet; `service` is the one that needs it.
pub fn certificate(peers: &Peers, service: &str, folder: &str, domain: &str) -> Result<(), Reason> {
    match exit_code(&sh(peers, CERTIFICATE, &[folder, domain])?) {
        0 => Ok(()),
        127 => Err(reason!(
            "server.program_missing",
            service = service,
            program = "openssl"
        )),
        _ => Err(reason!("server.tls_failed")),
    }
}

/// Write each `(path, content)`, making the folders first. The guest agent
/// creates files anyone in the machine may change, and some of these run as
/// root: so each is then left writable by root alone, and readable by root
/// alone when it holds one of `secrets`.
pub fn write_files(
    peers: &Peers,
    files: &[(String, String)],
    secrets: &[String],
) -> Result<(), Reason> {
    if files.is_empty() {
        return Ok(());
    }
    let mut mkdir = vec!["mkdir".to_owned(), "-p".to_owned()];
    mkdir.extend(files.iter().filter_map(|(path, _)| {
        std::path::Path::new(path)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .filter(|parent| !parent.is_empty())
    }));
    if mkdir.len() > 2 {
        execute(peers, &mkdir, &[])?;
    }
    for (path, content) in files {
        host(
            peers,
            "filesystem.write",
            data!({"path": path, "content": content}),
        )?;
    }
    let (private, shared): (Vec<_>, Vec<_>) = files.iter().partition(|(_, content)| {
        secrets
            .iter()
            .any(|secret| content.contains(secret.as_str()))
    });
    for (mode, some) in [("644", shared), ("600", private)] {
        if some.is_empty() {
            continue;
        }
        let mut chmod = vec!["chmod".to_owned(), mode.to_owned(), "--".to_owned()];
        chmod.extend(some.into_iter().map(|(path, _)| path.clone()));
        execute(peers, &chmod, &[])?;
    }
    Ok(())
}

/// Start `argv` as the service `id`, in `directory`: its pid, or the exit
/// code and output of a start that did not get as far as one.
pub fn start(
    peers: &Peers,
    id: &str,
    directory: &str,
    argv: &[String],
    secrets: &[String],
) -> Result<Result<i64, String>, Reason> {
    let mut parameters = vec![LOG_DIR, id, directory];
    parameters.extend(argv.iter().map(String::as_str));
    let said = sh_concealing(peers, START, &parameters, secrets)?;
    let stdout = said
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match stdout.trim().parse::<i64>() {
        Ok(pid) if exit_code(&said) == 0 && pid > 1 => Ok(Ok(pid)),
        _ => {
            let stderr = said
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(Err(format!("{stdout}{stderr}").trim().to_owned()))
        }
    }
}

/// Whether `pid` is alive and is still `program`.
pub fn alive(peers: &Peers, pid: i64, program: &str) -> Result<bool, Reason> {
    Ok(exit_code(&sh(peers, ALIVE, &[&pid.to_string(), program])?) == 0)
}

/// End `pid`: asked, then told.
pub fn stop(peers: &Peers, pid: i64) -> Result<(), Reason> {
    sh(peers, STOP, &[&pid.to_string()]).map(|_| ())
}

/// The last `lines` lines of service `id`'s output.
pub fn tail(peers: &Peers, id: &str, lines: u64) -> Result<Vec<String>, Reason> {
    let log = format!("{LOG_DIR}/{id}.log");
    let argv = ["tail", "-n", &lines.min(MOST_LINES).to_string(), &log].map(str::to_owned);
    let said = execute(peers, &argv, &[])?;
    Ok(said
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{HAS, PROGRAM_CHECK};
    use std::process::Command;

    #[test]
    fn program_check_resolves_path_and_treats_parameters_as_literals() {
        let check = |directory: &str, program: &str| {
            Command::new("/bin/sh")
                .args(["-c", PROGRAM_CHECK, "sh", directory, program])
                .output()
                .unwrap()
                .status
                .code()
                .unwrap()
        };
        assert_eq!(check("/", "sh"), 0);
        assert_eq!(check("/bin", "./sh"), 0);
        assert_eq!(check("/", "/bin/sh"), 0);
        assert_eq!(check("/", "beatbox-missing-program; exit 0"), 127);
        assert_eq!(check("/", "$(exit 0)"), 127);
        assert_eq!(check("/", "/"), 127);
        assert_eq!(check("/beatbox-nonexistent-directory", "sh"), 2);
    }

    #[test]
    fn has_says_what_exists_and_runs_nothing() {
        let said = Command::new("/bin/sh")
            .args([
                "-c",
                HAS,
                "sh",
                "sh",
                "/bin",
                "beatbox-missing",
                "/beatbox-nope",
                "x; echo pwned",
                "$(echo pwned)",
            ])
            .output()
            .unwrap();
        assert!(said.status.success());
        assert_eq!(String::from_utf8_lossy(&said.stdout), "sh\n/bin\n");
    }
}
