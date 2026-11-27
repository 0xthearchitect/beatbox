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
/// is the service's own. Until those execs are done the pid still reads as
/// this shell or `setsid`, and `ALIVE` would call a healthy service dead, so
/// wait for it, up to two seconds.
pub const START: &str = r#"mkdir -p "$1" || exit 1
log="$1/$2.log"
cd "$3" || exit 1
shift 3
setsid "$@" >"$log" 2>&1 </dev/null &
pid=$!
i=0
while [ "$i" -lt 40 ] && kill -0 "$pid" 2>/dev/null \
  && [ "$(tr '\0' '\n' 2>/dev/null <"/proc/$pid/cmdline" | head -n 1)" != "$1" ]; do
  sleep 0.05
  i=$((i+1))
done
echo "$pid""#;

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
# Read the rest before closing: closing on an unread answer resets the
# connection, and the site writes that error into the log people read.
cat <&3 >/dev/null
case "$code" in [23][0-9][0-9]) exit 0 ;; esac
exit 1"#;

/// What `GET $2` on port `$1` answers: `code=<status>`, then the first
/// 300 bytes of the body. Exit 1 when nothing accepts the connection.
pub const HTTP_SAID: &str = r#"exec 2>/dev/null 3<>"/dev/tcp/127.0.0.1/$1" || exit 1
printf 'GET %s HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n' "$2" >&3
IFS= read -r status <&3
status=${status%$'\r'}
code=${status#* }
echo "code=${code%% *}"
while IFS= read -r line <&3; do [ -z "${line%$'\r'}" ] && break; done
head -c 300 <&3
cat <&3 >/dev/null"#;

/// Every TCP port something listens on, as `<port> <pid>` lines, then
/// `--` and every process as `<pid> <ppid>`: enough to tell which ports a
/// service's own processes hold.
pub const LISTENING: &str = r#"ss -ltnpH 2>/dev/null | while read -r _ _ _ local _ users; do
  port=${local##*:}
  for pid in $(echo "$users" | grep -o 'pid=[0-9]*' | cut -d= -f2); do echo "$port $pid"; done
done
echo --
ps -eo pid=,ppid="#;

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

/// What preparing this guest would take, as `key=value` lines: the
/// system, whether it has apt, the free space, whether an install is under
/// way, whether its package sources answer, and for packages `"$@"` those
/// apt does not have, and the room and download the rest need. Read, never
/// changed: it never runs `apt-get update`, so a guest that never had one
/// says `lists=no` and what it has stays unknown.
pub const PLAN: &str = r#"export LC_ALL=C
os=/etc/os-release
printf 'system=%s\n' "$(sed -n 's/^PRETTY_NAME=//p' "$os" 2>/dev/null | tr -d '"')"
kin=" $(sed -n 's/^ID=//p;s/^ID_LIKE=//p' "$os" 2>/dev/null | tr -d '"' | tr '\n' ' ') "
case "$kin" in *" debian "*|*" ubuntu "*) ;; *) exit 0 ;; esac
command -v apt-get >/dev/null 2>&1 || exit 0
echo apt=yes
printf 'free_kb=%s\n' "$(df -Pk / | awk 'NR==2 {print $4}')"
d=/var/log/beatbox
[ "$(cat "$d/prepare.state" 2>/dev/null)" = running ] \
  && kill -0 "$(cat "$d/prepare.pid" 2>/dev/null)" 2>/dev/null && echo busy=yes
source=$(cat /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources 2>/dev/null \
  | grep -v '^[[:space:]]*#' | sed -n 's#.*\(https\{0,1\}\)://\([A-Za-z0-9.-]*\).*#\1 \2#p' | head -n 1)
if [ -z "$source" ]; then
  echo reach=no
else
  port=80
  case "$source" in https*) port=443 ;; esac
  if timeout 4 bash -c 'exec 3<>"/dev/tcp/$1/$2"' bash "${source#* }" "$port" 2>/dev/null; then
    echo reach=yes
  else
    echo reach=no
  fi
fi
ls /var/lib/apt/lists 2>/dev/null | grep -q '_Packages' || { echo lists=no; exit 0; }
echo lists=yes
absent=
for package; do
  candidate=$(apt-cache policy "$package" 2>/dev/null | sed -n 's/^ *Candidate: //p')
  if [ -z "$candidate" ] || [ "$candidate" = "(none)" ]; then
    echo "absent=$package"
    absent=yes
  fi
done
[ -n "$absent" ] && exit 0
new=$(apt-get install -s --no-install-recommends "$@" 2>/dev/null | sed -n 's/^Inst \([^ ]*\).*/\1/p')
[ -z "$new" ] && exit 0
printf 'room_kb=%s\n' "$(apt-cache show --no-all-versions $new 2>/dev/null | awk '/^Installed-Size:/ {s+=$2} END {print s+0}')"
printf 'get_kb=%s\n' "$(apt-get install --print-uris -qq --no-install-recommends "$@" 2>/dev/null | awk '{s+=$3} END {print int(s/1024)}')""#;

/// Install packages `"$@"` after `$1` (the script that does it, [`INSTALL`])
/// and `$2` (units to keep off, spaces between), detached from the agent: an
/// install takes minutes, and the Host answers one thing at a time. 3 when
/// one is already under way. [`PREPARING`] follows it.
pub const APPLY: &str = r#"d=/var/log/beatbox
mkdir -p "$d" || exit 1
if [ "$(cat "$d/prepare.state" 2>/dev/null)" = running ] \
  && kill -0 "$(cat "$d/prepare.pid" 2>/dev/null)" 2>/dev/null; then exit 3; fi
work=$1
units=$2
shift 2
printf '%s\n' "$@" >"$d/prepare.packages"
echo running >"$d/prepare.state"
setsid /bin/sh -c "$work" sh "$units" "$@" >"$d/prepare.log" 2>&1 </dev/null &
echo $! >"$d/prepare.pid""#;

/// The install itself: packages `"$@"` after `$1`, the units to keep off.
/// No service of theirs starts while it runs (policy-rc.d, unless the guest
/// already has one of its own), and theirs stay off after: the Server runs
/// its own, on the same port.
pub const INSTALL: &str = r#"export DEBIAN_FRONTEND=noninteractive LC_ALL=C
set -f
d=/var/log/beatbox
units=$1
shift
mine=
if [ ! -e /usr/sbin/policy-rc.d ]; then
  printf '#!/bin/sh\nexit 101\n' >/usr/sbin/policy-rc.d && chmod 755 /usr/sbin/policy-rc.d && mine=yes
fi
wait=DPkg::Lock::Timeout=120
apt-get -o "$wait" update && apt-get -o "$wait" install -y --no-install-recommends -o Dpkg::Options::=--force-confold "$@"
code=$?
[ -n "$mine" ] && rm -f /usr/sbin/policy-rc.d
if [ "$code" -ne 0 ]; then
  echo failed >"$d/prepare.state"
  exit "$code"
fi
for unit in $units; do
  systemctl disable --now "$unit" >/dev/null 2>&1
done
echo done >"$d/prepare.state""#;

/// How the last install went, as `key=value` lines: `state` (none, running,
/// done or failed: running with its process gone is failed), each package
/// installed with its version once done, and the end of its log.
pub const PREPARING: &str = r#"d=/var/log/beatbox
state=$(cat "$d/prepare.state" 2>/dev/null) || { echo state=none; exit 0; }
if [ "$state" = running ] && ! kill -0 "$(cat "$d/prepare.pid" 2>/dev/null)" 2>/dev/null; then
  state=failed
fi
echo "state=$state"
if [ "$state" = done ]; then
  while IFS= read -r package; do
    dpkg-query -W -f='installed=${Package} ${Version}\n' "$package" 2>/dev/null
  done <"$d/prepare.packages"
fi
tail -n 60 "$d/prepare.log" 2>/dev/null | sed 's/^/line=/'
exit 0"#;

/// Where copies wait inside the guest on their way in or out, one folder
/// each. Root's alone, but crossable, so PostgreSQL can write its dump.
pub const COPY_STAGE: &str = "/var/lib/beatbox/copies";

/// Make `$1`, a fresh folder under [`COPY_STAGE`].
pub const STAGE: &str = r#"set -eu
case "$1" in /var/lib/beatbox/copies/?*) ;; *) exit 2;; esac
umask 077
mkdir -p /var/lib/beatbox/copies
chmod 711 /var/lib/beatbox/copies
rm -rf "$1"
mkdir "$1""#;

/// Pack the paths after `$1` into `$1/copy`. The service is stopped: these
/// are files a running program may be halfway through writing. A path not
/// there yet is left out.
pub const PACK: &str = r#"set -eu
stage=$1
shift
tar -czf "$stage/copy" --ignore-failed-read "$@" 2>"$stage/said" || { cat "$stage/said" >&2; exit 1; }"#;

/// Size and sha256 of `$1`, as `bytes=` and `sha=` lines.
pub const SUM: &str = r#"set -eu
printf 'bytes=%s\nsha=%s\n' "$(wc -c <"$1")" "$(sha256sum "$1" | cut -d' ' -f1)""#;

/// Piece `$2` (from 0) of `$1`, [`PIECE_BYTES`] long, in base64.
pub const PIECE: &str = r#"dd if="$1" bs=4194304 skip="$2" count=1 status=none | base64 -w0"#;

/// The bytes of a piece read from the guest at a time: in base64 well under
/// the 16 MB of output the agent keeps from one run. Matches `bs=` in
/// [`PIECE`].
pub const PIECE_BYTES: u64 = 4 * 1024 * 1024;

/// Add the base64 in `$1/piece` to `$1/copy`.
pub const APPEND: &str = r#"set -eu
base64 -d "$1/piece" >>"$1/copy"
rm -f "$1/piece""#;

/// Put `$1/copy` back over the paths after `$1`, with the service stopped.
/// All of it is unpacked first, so a damaged copy changes nothing; what was
/// there waits in `$1/old` until the Server deletes the stage.
pub const UNPACK: &str = r#"set -eu
stage=$1
shift
mkdir "$stage/new" "$stage/old"
tar -xzf "$stage/copy" -C "$stage/new"
for path; do
  if [ -e "$path" ] || [ -L "$path" ]; then
    mkdir -p "$stage/old${path%/*}"
    mv "$path" "$stage/old$path"
  fi
  if [ -e "$stage/new$path" ] || [ -L "$stage/new$path" ]; then
    mkdir -p "${path%/*}"
    mv "$stage/new$path" "$path"
  fi
done"#;

/// Delete the stage `$1`, and nothing that is not one.
pub const UNSTAGE: &str = r#"case "$1" in /var/lib/beatbox/copies/?*) rm -rf "$1";; esac"#;

/// Kilobytes the paths given take, those that are there.
pub const SIZE: &str = r#"total=0
for path; do
  [ -e "$path" ] || continue
  kb=$(du -sk -- "$path" | cut -f1)
  total=$((total + kb))
done
echo "$total""#;

/// `argv`, given up to `seconds` instead of the Host's usual wait: a dump
/// or a large copy takes longer than a check.
pub fn execute_for(
    peers: &Peers,
    argv: &[String],
    secrets: &[String],
    seconds: u32,
) -> Result<Data, Reason> {
    let (program, arguments) = argv
        .split_first()
        .expect("a definition's argv is never empty");
    let mut asked = data!({"command": program, "arguments": arguments, "timeout": seconds});
    if !secrets.is_empty() {
        asked.insert("conceal".into(), secrets.into());
    }
    host(peers, "process.execute", asked)
}

/// `script` like [`sh`], given up to `seconds`.
pub fn sh_for(
    peers: &Peers,
    script: &str,
    parameters: &[&str],
    seconds: u32,
) -> Result<Data, Reason> {
    let mut argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
        "sh".to_owned(),
    ];
    argv.extend(parameters.iter().map(|p| (*p).to_owned()));
    execute_for(peers, &argv, &[], seconds)
}

/// Write `content` to `path` in the guest.
pub fn write(peers: &Peers, path: &str, content: &str) -> Result<(), Reason> {
    host(
        peers,
        "filesystem.write",
        data!({"path": path, "content": content}),
    )
    .map(|_| ())
}

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

/// `script` run by `/bin/sh` with `parameters` as `"$@"`.
pub fn sh(peers: &Peers, script: &str, parameters: &[&str]) -> Result<Data, Reason> {
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

/// What a run exited with; -1 when it did not say.
pub fn exit_code(data: &Data) -> i64 {
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

/// What `GET path` on `port` answers: its status code and the start of its
/// body, or `None` when nothing accepts the connection.
pub fn http_said(peers: &Peers, port: u16, path: &str) -> Result<Option<(u16, String)>, Reason> {
    let argv: Vec<String> = ["timeout", CHECK_SECONDS, "bash", "-c", HTTP_SAID, "bash"]
        .map(str::to_owned)
        .into_iter()
        .chain([port.to_string(), path.to_owned()])
        .collect();
    let said = execute(peers, &argv, &[])?;
    let out = said
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (first, body) = out.split_once('\n').unwrap_or((out, ""));
    Ok(first
        .strip_prefix("code=")
        .and_then(|code| code.trim().parse().ok())
        .map(|code| (code, body.to_owned())))
}

/// The TCP ports held by process `pid` or any process under it.
pub fn ports_of(peers: &Peers, pid: i64) -> Result<Vec<u16>, Reason> {
    let said = sh(peers, LISTENING, &[])?;
    let out = said
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (listening, table) = out.split_once("--").unwrap_or((out, ""));
    let pair = |line: &str| {
        let mut words = line.split_whitespace().map(str::parse::<i64>);
        Some((words.next()?.ok()?, words.next()?.ok()?))
    };
    let parents: Vec<(i64, i64)> = table.lines().filter_map(pair).collect();
    let mine = |mut each: i64| {
        // A parent chain is short; the bound only stops a table that loops.
        for _ in 0..64 {
            if each == pid {
                return true;
            }
            match parents.iter().find(|(child, _)| *child == each) {
                Some((_, parent)) if *parent > 1 => each = *parent,
                _ => return false,
            }
        }
        false
    };
    let mut ports: Vec<u16> = listening
        .lines()
        .filter_map(pair)
        .filter(|(_, owner)| mine(*owner))
        .filter_map(|(port, _)| u16::try_from(port).ok())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
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
