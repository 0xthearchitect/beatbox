//! The architecture's test: a service BeatBox has never seen, on a port it
//! has never heard of, described in a JSON written here and nowhere else,
//! runs for real -- started, checked, talked to, asked, read, stopped --
//! and not one line of `crates/server/src` knows what it is.
//!
//! The Host is this machine: a test provider runs each argv here, so the
//! Server's real scripts start a real `python3` on a real socket. Linux
//! only, like the `process` runtime.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use beatbox_engine::{
    Action, ActionOutcome, Data, Isolation, Observation, ProviderError, ProviderSpec, RuntimeParts,
    SandboxConfig, SandboxProvider, SandboxRegistry, SandboxRuntime, data,
};
use beatbox_server::ServerSandbox;
use beatbox_server::process::LOG_DIR;
use serde_json::{Value, json};

/// The service: an echo over TCP, on 3610 unless told otherwise. Data,
/// like any definition -- the program is Python's, given on its command
/// line, and the Server reads none of it.
fn parrot() -> Value {
    json!({
        "id": "parrot",
        "version": "1",
        "name": "Parrot",
        "description": "Says back whatever it is told, one line per connection.",
        "author": "a test",
        "runtime": {
            "type": "process",
            "command": ["python3", "-c", SERVE, "{port}", "{greeting}"]
        },
        "configuration": {
            "port": {"kind": "number", "default": 3610},
            "greeting": {"kind": "text", "default": "squawk"},
            "notes": {"kind": "text", "default": "/tmp"}
        },
        "interfaces": [{"transport": "tcp", "port": "{port}"}],
        "files": [{"path": "{notes}/{_id}.txt", "content": "listening on {port}\n"}],
        "health": {"type": "tcp", "port": "{port}"},
        "capabilities": [{
            "capability": "parrot.say",
            "inputs": [{"name": "words", "kind": "text"}],
            "outputs": [{"name": "stdout", "kind": "text"}],
            "run": ["python3", "-c", ASK, "{port}", "{words}"]
        }]
    })
}

const SERVE: &str = "import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', int(sys.argv[1])))
s.listen()
print('ready', flush=True)
while True:
    c, _ = s.accept()
    said = c.recv(1024)
    print('heard', said.decode().strip(), flush=True)
    c.sendall(sys.argv[2].encode() + b' ' + said)
    c.close()";

const ASK: &str = "import socket, sys
c = socket.create_connection(('127.0.0.1', int(sys.argv[1])))
c.sendall(sys.argv[2].encode())
print(c.recv(1024).decode(), end='')";

/// A Host that is this machine. `LOG_DIR` is the one thing translated: a
/// test may not write to `/var/log`.
struct ThisMachine {
    logs: PathBuf,
}

impl SandboxProvider for ThisMachine {
    fn create(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn start(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn stop(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn destroy(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn observe(&mut self) -> Observation {
        Observation {
            alive: Some(true),
            facts: data!({"family": "linux", "addresses": ["127.0.0.1", "10.20.0.10"]}),
            ..Observation::default()
        }
    }
    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let logs = self.logs.to_string_lossy().into_owned();
        let here = |text: &str| text.replace(LOG_DIR, &logs);
        match action.capability() {
            "network.interfaces" => Ok(
                data!({"interfaces": [{"name": "lab", "addresses": [{"address": "10.20.0.10"}]}]}),
            ),
            "process.execute" => {
                let command = here(action.arguments["command"].as_str().unwrap());
                let arguments: Vec<String> = action.arguments["arguments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| here(a.as_str().unwrap()))
                    .collect();
                let out = Command::new(command).args(arguments).output().unwrap();
                Ok(data!({
                    "pid": 0,
                    "exit_code": out.status.code().unwrap_or(-1),
                    "stdout": String::from_utf8_lossy(&out.stdout),
                    "stderr": String::from_utf8_lossy(&out.stderr),
                    "truncated": false,
                }))
            }
            "filesystem.write" => {
                let path = action.arguments["path"].as_str().unwrap();
                let content = action.arguments["content"].as_str().unwrap();
                std::fs::write(path, content).unwrap();
                Ok(data!({"path": path, "bytes": content.len()}))
            }
            other => panic!("the Server asked this machine for {other}"),
        }
    }
}

/// Stops the service however the test ends: a failed assertion must not
/// leave a real python3 listening on this machine.
struct StopOnDrop(beatbox_engine::Sandbox);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = self.0.act("service.stop", data!({"service": "polly"}));
    }
}

fn have(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .is_ok_and(|out| out.status.success())
}

fn ok(outcome: ActionOutcome) -> Data {
    assert!(outcome.ok, "{:?}", outcome.reason);
    outcome.data
}

#[test]
fn a_service_nobody_wrote_code_for_runs_for_real() {
    if !["python3", "setsid", "timeout", "bash"]
        .iter()
        .all(|p| have(p))
    {
        eprintln!("skipped: needs python3, setsid, timeout and bash");
        return;
    }
    let room = std::env::temp_dir().join(format!("bbx-custom-{}", std::process::id()));
    let logs = room.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let mut registry = SandboxRegistry::default();
    let host_logs = logs.clone();
    registry
        .register(ProviderSpec::new(
            "here",
            "1",
            &[
                "process.execute",
                "filesystem.read",
                "filesystem.write",
                "network.interfaces",
            ],
            Isolation::None,
            move |_, _, _| {
                Box::new(ThisMachine {
                    logs: host_logs.clone(),
                })
            },
        ))
        .unwrap();
    registry.register(ServerSandbox::spec()).unwrap();
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    let host = runtime
        .create(SandboxConfig::new("here").unwrap().named("box"))
        .unwrap();
    host.start().unwrap();
    let server = runtime
        .create(
            SandboxConfig::new("server")
                .unwrap()
                .with_settings(data!({"workspace": room.join("srv").to_string_lossy()}))
                .using("host", "box"),
        )
        .unwrap();
    server.start().unwrap();
    let _guard = StopOnDrop(server.clone());
    // 3610 is the definition's; a free one here, so the test never collides.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let act = |capability: &str, arguments: Data| server.act(capability, arguments).unwrap();

    ok(act(
        "service.create",
        data!({
            "definition": parrot(),
            "name": "polly",
            "config": {"port": port, "notes": room.to_string_lossy()}
        }),
    ));
    let started = ok(act("service.start", data!({"service": "polly"})));
    assert_eq!(started["state"], "running");
    assert_eq!(
        started["health"], "healthy",
        "its tcp check found it listening"
    );

    // A stranger on the network, talking to it.
    let mut talk = TcpStream::connect(("127.0.0.1", port)).unwrap();
    talk.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    talk.write_all(b"hello").unwrap();
    let mut heard = String::new();
    talk.read_to_string(&mut heard).unwrap();
    assert_eq!(heard, "squawk hello");

    // Its own capability, through the Server.
    let said = ok(act(
        "service.call",
        data!({"service": "polly", "capability": "parrot.say", "arguments": {"words": "pieces of eight"}}),
    ));
    assert_eq!(said["stdout"], "squawk pieces of eight");

    // Where it answers, its file, its output.
    let facts = server.refresh().unwrap().facts;
    assert_eq!(facts["endpoints"][0]["port"], port);
    assert_eq!(facts["endpoints"][0]["transport"], "tcp");
    // Its loopback is never where it answers for anybody else.
    assert_eq!(facts["endpoints"][0]["address"], "10.20.0.10");
    let id = format!("{}-polly", server.id());
    assert_eq!(
        std::fs::read_to_string(room.join(format!("{id}.txt"))).unwrap(),
        format!("listening on {port}\n")
    );
    let logs = ok(act(
        "service.logs",
        data!({"service": "polly", "lines": 10}),
    ));
    let lines: Vec<&str> = logs["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(
        lines.contains(&"ready") && lines.contains(&"heard hello"),
        "{lines:?}"
    );

    let stopped = ok(act("service.stop", data!({"service": "polly"})));
    assert_eq!(stopped["state"], "stopped");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "nothing listens once it is stopped"
    );
    drop(_guard);
    drop(server);
    drop(host);
    let _ = std::fs::remove_dir_all(&room);
}

/// And the other half of the test: the Server's code never mentions it.
/// Comments may -- the documentation uses 3610 as its example.
#[test]
fn the_servers_code_does_not_know_this_service() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(source).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for (number, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for word in ["3610", "parrot", "polly", "squawk"] {
                assert!(
                    !line.contains(word),
                    "{}:{}: {word}",
                    path.display(),
                    number + 1
                );
            }
        }
    }
}
