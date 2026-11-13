//! The Server and its `process` runtime, against a Host that pretends.
//!
//! The pretend Host keeps a process table and answers the Server's three
//! scripts like a Linux guest, so a service can start, stop, die, or have
//! its pid reused, and every argv sent can be read back.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use beatbox_engine::{
    Action, ActionOutcome, Data, Isolation, Observation, ProviderError, ProviderSpec, Recorder,
    RuntimeParts, Sandbox, SandboxConfig, SandboxProvider, SandboxRegistry, SandboxRuntime, data,
};
use beatbox_server::process::{
    ALIVE, CERTIFICATE, HAS, LOG_DIR, PROGRAM_CHECK, START, STOP, TCP_CHECK,
};
use beatbox_server::{CAPABILITIES, ServerSandbox, actions};
use serde_json::{Value, json};

#[derive(Default)]
struct Guest {
    /// Every action the Server asked of the Host, in order.
    calls: Vec<(String, Data)>,
    /// pid -> the program it is.
    running: BTreeMap<i64, String>,
    next_pid: i64,
    files: BTreeMap<String, String>,
    family: &'static str,
    /// What a start says instead of a pid.
    refuse_start: Option<&'static str>,
    missing_program: Option<&'static str>,
    refuse_prepare: Option<&'static str>,
    /// Whether every health check fails. A check passes otherwise, as long
    /// as something is running.
    sick: bool,
    /// The guest disappeared before its lifecycle was refreshed.
    unavailable: bool,
    /// Up, but its agent not yet: a machine still booting.
    no_agent: bool,
    /// When it last went back to a save point, as the Host says it.
    went_back_at: Option<u64>,
    addresses: Option<Vec<String>>,
    /// Every health check run, as argv after `timeout 3`.
    checks: Vec<Vec<String>>,
    /// What a service's log says, when not the usual.
    log: Option<&'static str>,
    /// The programs and paths it has, for `server.check`.
    has: Vec<&'static str>,
}

type Shared = Arc<Mutex<Guest>>;

struct PretendHost {
    guest: Shared,
}

fn words(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| w.as_str().unwrap_or_default().to_owned())
        .collect()
}

impl SandboxProvider for PretendHost {
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
        let guest = self.guest.lock().unwrap();
        Observation {
            alive: Some(true),
            facts: data!({"family": guest.family, "addresses": guest.addresses.clone().unwrap_or_else(|| vec!["10.10.10.10".into()]), "went_back_at": guest.went_back_at}),
            ..Observation::default()
        }
    }
    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let mut guest = self.guest.lock().unwrap();
        if guest.unavailable {
            return Err(ProviderError::new(beatbox_engine::reason!(
                "engine.not_running"
            )));
        }
        if guest.no_agent {
            return Err(ProviderError::new(beatbox_engine::reason!("host.no_agent")));
        }
        guest
            .calls
            .push((action.capability().to_owned(), action.arguments.clone()));
        let done = |code: i64, stdout: &str| {
            Ok(
                data!({"pid": 9, "exit_code": code, "stdout": stdout, "stderr": "", "truncated": false}),
            )
        };
        match action.capability() {
            "network.interfaces" => {
                let addresses = guest
                    .addresses
                    .clone()
                    .unwrap_or_else(|| vec!["10.10.10.10".into()]);
                Ok(
                    data!({"interfaces": [{"name": "eth0", "addresses": addresses.iter().map(|address| json!({"address": address})).collect::<Vec<_>>()}]}),
                )
            }
            "filesystem.write" => {
                let path = action.arguments["path"].as_str().unwrap().to_owned();
                let content = action.arguments["content"].as_str().unwrap().to_owned();
                let bytes = content.len();
                guest.files.insert(path.clone(), content);
                Ok(data!({"path": path, "bytes": bytes}))
            }
            "process.execute" => {
                let command = action.arguments["command"].as_str().unwrap();
                let arguments = words(&action.arguments["arguments"]);
                match command {
                    "/bin/sh" => {
                        let script = arguments[1].as_str();
                        let parameters = &arguments[3..];
                        if script == PROGRAM_CHECK {
                            done(
                                if guest.missing_program == Some(parameters[1].as_str()) {
                                    127
                                } else {
                                    0
                                },
                                "",
                            )
                        } else if script == START {
                            if let Some(said) = guest.refuse_start {
                                return done(127, said);
                            }
                            guest.next_pid += 1;
                            let pid = 1000 + guest.next_pid;
                            guest.running.insert(pid, parameters[3].clone());
                            done(0, &format!("{pid}\n"))
                        } else if script == ALIVE {
                            let pid: i64 = parameters[0].parse().unwrap();
                            let same = guest.running.get(&pid) == Some(&parameters[1]);
                            done(if same { 0 } else { 1 }, "")
                        } else if script == HAS {
                            let found: Vec<&str> = parameters
                                .iter()
                                .map(String::as_str)
                                .filter(|asked| guest.has.contains(asked))
                                .collect();
                            done(0, &found.join("\n"))
                        } else if script == CERTIFICATE {
                            done(
                                if guest.missing_program == Some("openssl") {
                                    127
                                } else {
                                    0
                                },
                                "",
                            )
                        } else if script == STOP {
                            let pid: i64 = parameters[0].parse().unwrap();
                            guest.running.remove(&pid);
                            done(0, "")
                        } else {
                            panic!("a script the Server does not have: {script}")
                        }
                    }
                    "mkdir" | "rm" | "chmod" => done(0, ""),
                    "timeout" => {
                        assert_eq!(arguments[0], "3", "a check is always bounded");
                        let checked = arguments[1..].to_vec();
                        if checked.get(2).map(String::as_str) == Some("check") {
                            return match guest.refuse_prepare {
                                Some(why) => done(1, why),
                                None => done(0, ""),
                            };
                        }
                        // Definitions may also supply their own bash command check.
                        guest.checks.push(checked);
                        let well = !guest.sick && !guest.running.is_empty();
                        done(if well { 0 } else { 1 }, "")
                    }
                    "tail" => done(
                        0,
                        guest
                            .log
                            .unwrap_or("Serving HTTP on 0.0.0.0 port 8000\nGET / 200\n"),
                    ),
                    // A service's own program, or one of its capabilities.
                    program if program.starts_with("/srv/") => {
                        done(0, &format!("done {}\n", arguments.join(" ")))
                    }
                    other => panic!("the Server ran {other} directly"),
                }
            }
            other => panic!("the Server asked the Host for {other}"),
        }
    }
}

fn pretend_host(guest: &Shared) -> ProviderSpec {
    let guest = Arc::clone(guest);
    ProviderSpec::new(
        "pretend",
        "1",
        &[
            "process.execute",
            "filesystem.read",
            "filesystem.write",
            "network.interfaces",
        ],
        Isolation::None,
        move |_, _, _| {
            Box::new(PretendHost {
                guest: Arc::clone(&guest),
            })
        },
    )
}

fn room() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let room = std::env::temp_dir().join(format!(
        "bbx-server-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&room);
    room
}

struct World {
    /// Held, not read: peers find each other through it, weakly.
    _runtime: SandboxRuntime,
    guest: Shared,
    log: Recorder,
    host: Sandbox,
    server: Sandbox,
    room: PathBuf,
}

fn runtime(guest: &Shared) -> SandboxRuntime {
    let mut registry = SandboxRegistry::default();
    registry.register(pretend_host(guest)).unwrap();
    registry.register(ServerSandbox::spec()).unwrap();
    SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    })
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.room);
    }
}

fn world_with(family: &'static str, settings: Data) -> World {
    let guest: Shared = Arc::new(Mutex::new(Guest {
        family,
        ..Guest::default()
    }));
    let runtime = runtime(&guest);
    let log = Recorder::default();
    runtime.events().subscribe("service.*", log.sink()).unwrap();
    let host = runtime
        .create(SandboxConfig::new("pretend").unwrap().named("web"))
        .unwrap();
    host.start().unwrap();
    let room = room();
    let mut settings = settings;
    settings.insert("workspace".into(), json!(room.to_string_lossy()));
    settings.entry("start_period_s").or_insert(json!(0.3));
    let server = runtime
        .create(
            SandboxConfig::new("server")
                .unwrap()
                .named("srv")
                .with_settings(settings)
                .using("host", "web"),
        )
        .unwrap();
    server.start().unwrap();
    World {
        _runtime: runtime,
        guest,
        log,
        host,
        server,
        room,
    }
}

fn world() -> World {
    world_with("linux", Data::new())
}

#[test]
fn explicit_list_health_detects_live_but_unresponsive_services_without_observe_io() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site", "config": {"port": 8080}}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    let first = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(first["services"][0]["health"], "healthy");
    w.guest.lock().unwrap().sick = true;
    let cached = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(cached["services"][0]["health"], "healthy");
    let checked = ok(act(
        &w.server,
        "service.list",
        data!({"check_health": true}),
    ));
    assert_eq!(checked["services"][0]["health"], "unhealthy");
    assert!(checked["services"][0]["health_checked_at"].is_u64());
    let count = w.guest.lock().unwrap().checks.len();
    w.server.refresh().unwrap();
    assert_eq!(w.guest.lock().unwrap().checks.len(), count);
    w.guest.lock().unwrap().sick = false;
    let healthy = ok(act(
        &w.server,
        "service.list",
        data!({"check_health": true}),
    ));
    assert_eq!(healthy["services"][0]["health"], "healthy");
}

#[test]
fn definition_update_is_explicit_stopped_and_preserves_secrets_and_identity() {
    let w = world();
    let mut definition = serde_json::to_value(beatbox_server::builtins()[3].clone()).unwrap();
    definition["version"] = json!("old");
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": definition, "name": "db", "config": {"initial_password": "keep-secret", "port": 5544}}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "db"})));
    assert!(
        !act(
            &w.server,
            "service.configure",
            data!({"service": "db", "config": {}, "update_definition": true})
        )
        .ok
    );
    ok(act(&w.server, "service.stop", data!({"service": "db"})));
    ok(act(
        &w.server,
        "service.configure",
        data!({"service": "db", "config": {}, "update_definition": true}),
    ));
    let current = ok(act(&w.server, "service.describe", data!({"service": "db"})));
    assert_eq!(current["definition"]["version"], "5");
    assert_eq!(current["config"]["port"], 5544);
    assert_eq!(
        current["config"]["initial_password"],
        beatbox_server::definition::SECRET_SHOWN
    );
    ok(act(&w.server, "service.start", data!({"service": "db"})));
    let argv = last_sh(&w.guest, START);
    assert_eq!(argv.last().unwrap(), "keep-secret");
}

/// A successful answer, holding what `actions()` promises for it.
fn ok(outcome: ActionOutcome) -> Data {
    assert!(outcome.ok, "{:?}", outcome.reason);
    let spec = actions()
        .into_iter()
        .find(|spec| spec.capability == outcome.capability())
        .unwrap_or_else(|| panic!("{} has no spec", outcome.capability()));
    spec.assert_outputs(&outcome.data);
    outcome.data
}

fn no(outcome: ActionOutcome) -> (String, BTreeMap<String, String>) {
    assert!(!outcome.ok, "{:?}", outcome.data);
    let why = outcome.reason.unwrap();
    (why.code, why.values)
}

fn act(server: &Sandbox, capability: &str, arguments: Data) -> ActionOutcome {
    server.act(capability, arguments).unwrap()
}

/// The parameters of the last `/bin/sh` the Server ran with `script`.
fn last_sh(guest: &Shared, script: &str) -> Vec<String> {
    let guest = guest.lock().unwrap();
    let (_, arguments) = guest
        .calls
        .iter()
        .rev()
        .find(|(cap, args)| {
            cap == "process.execute"
                && words(&args["arguments"]).get(1).map(String::as_str) == Some(script)
        })
        .expect("the script was run");
    words(&arguments["arguments"])[3..].to_vec()
}

#[test]
fn every_capability_says_what_it_takes_and_gives_back() {
    let described: Vec<String> = actions().into_iter().map(|spec| spec.capability).collect();
    for capability in CAPABILITIES {
        if !capability.starts_with("lifecycle.") {
            assert!(
                described.iter().any(|d| d == capability),
                "{capability} has no spec"
            );
        }
    }
}

#[test]
fn database_and_email_use_the_existing_process_lifecycle() {
    for (definition, config, protocol, port) in [
        (
            "postgresql",
            data!({"initial_password": "test-only; $(false)", "port": 5544}),
            "postgresql",
            5544,
        ),
        (
            "smtp",
            data!({"port": 2525, "web_port": 8085}),
            "smtp",
            2525,
        ),
    ] {
        let w = world();
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": definition, "name": "lab", "config": config}),
        ));
        let started = ok(act(&w.server, "service.start", data!({"service": "lab"})));
        assert_eq!(started["state"], "running");
        let listed = ok(act(&w.server, "service.list", Data::new()));
        let service = &listed["services"][0];
        assert_eq!(service["endpoints"][0]["protocol"], protocol);
        assert_eq!(service["endpoints"][0]["port"], port);
        let argv = last_sh(&w.guest, START);
        if definition == "postgresql" {
            assert_eq!(
                service["config"]["initial_password"],
                beatbox_server::definition::SECRET_SHOWN
            );
            assert_eq!(argv[3], "bash");
            assert_eq!(argv.last().unwrap(), "test-only; $(false)");
        } else {
            assert_eq!(argv[3], "mailpit");
            assert_eq!(service["endpoints"][1]["protocol"], "http");
            assert_eq!(service["endpoints"][1]["port"], 8085);
            // The inbox and another website cannot claim the same port.
            ok(act(
                &w.server,
                "service.create",
                data!({"definition": "http-static", "name": "site", "config": {"port": 8085}}),
            ));
            let refused = act(&w.server, "service.start", data!({"service": "site"}));
            assert!(!refused.ok);
            assert_eq!(refused.reason.unwrap().code, "server.port_taken");
        }
        let stopped = ok(act(&w.server, "service.stop", data!({"service": "lab"})));
        assert_eq!(stopped["state"], "stopped");
        assert!(w.guest.lock().unwrap().running.is_empty());
        ok(act(&w.server, "service.start", data!({"service": "lab"})));
    }
}

/// The Host says every argv it runs in `process.created`, and that event goes
/// to the history and to every open page. So whatever carries a secret goes
/// with `conceal`, and what the service prints back is masked by the Server.
#[test]
fn a_secret_is_run_but_never_said() {
    let w = world();
    let secret = "s3cret-pw";
    ok(act(
        &w.server,
        "service.create",
        data!({
            "name": "app",
            "config": {"token": secret},
            "definition": {
                "id": "app", "version": "1", "name": "App",
                "runtime": {"type": "process", "command": ["/srv/app", "--token={token}"]},
                "configuration": {"token": {"kind": "secret"}},
                "health": {"type": "command", "command": ["/srv/ping", "{token}"]},
                "capabilities": [{"capability": "app.use", "run": ["/srv/use", "{token}"]}]
            }
        }),
    ));
    w.guest.lock().unwrap().log = Some("starting with s3cret-pw\n");
    ok(act(&w.server, "service.start", data!({"service": "app"})));
    let used = ok(act(
        &w.server,
        "service.call",
        data!({"service": "app", "capability": "app.use"}),
    ));
    assert_eq!(used["stdout"], "done ••••\n");
    let logs = ok(act(&w.server, "service.logs", data!({"service": "app"})));
    assert_eq!(logs["lines"], json!(["starting with ••••"]));

    let guest = w.guest.lock().unwrap();
    let carrying: Vec<&Data> = guest
        .calls
        .iter()
        .filter(|(capability, _)| capability == "process.execute")
        .map(|(_, arguments)| arguments)
        .filter(|arguments| arguments["arguments"].to_string().contains(secret))
        .collect();
    // The start, the health check and the capability.
    assert_eq!(carrying.len(), 3);
    for arguments in carrying {
        assert_eq!(arguments["conceal"], json!([secret]));
    }
}

/// Removing keeps what the service left where it ran -- a database made
/// again with the same name finds its data -- unless asked to delete it:
/// then its own folders (the ones named by its id) and its log go, and
/// nothing else. A machine that cannot be asked keeps the service.
#[test]
fn removing_keeps_the_data_unless_asked_to_delete_it() {
    let w = world();
    let make = |name: &str| {
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": "postgresql", "name": name, "config": {"initial_password": "pw"}}),
        ));
        ok(act(&w.server, "service.start", data!({"service": name})));
    };
    let rm_calls = |w: &World| {
        w.guest
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(capability, arguments)| {
                capability == "process.execute" && arguments["command"] == "rm"
            })
            .map(|(_, arguments)| words(&arguments["arguments"]))
            .collect::<Vec<_>>()
    };
    let id = format!("{}-db", w.server.id());
    let own = vec![
        format!("/etc/beatbox/{id}"),
        format!("/var/lib/postgresql/beatbox/{id}"),
        format!("/var/log/beatbox/{id}.log"),
    ];

    make("db");
    let described = ok(act(&w.server, "service.describe", data!({"service": "db"})));
    assert_eq!(described["data"], json!(own));
    ok(act(&w.server, "service.remove", data!({"service": "db"})));
    assert!(rm_calls(&w).is_empty(), "kept by default");

    make("db");
    w.guest.lock().unwrap().unavailable = true;
    let (code, _) = no(act(
        &w.server,
        "service.remove",
        data!({"service": "db", "delete_data": true}),
    ));
    assert_eq!(code, "server.delete_data_needs_host");
    w.guest.lock().unwrap().unavailable = false;
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["service"], "db", "not forgotten");

    let removed = ok(act(
        &w.server,
        "service.remove",
        data!({"service": "db", "delete_data": true}),
    ));
    assert_eq!(removed["deleted"], json!(own));
    let mut expected = vec!["-rf".to_owned(), "--".to_owned()];
    expected.extend(own);
    assert_eq!(rm_calls(&w), [expected]);
}

/// A start the Server tries on its own, restoring, that does not happen is
/// kept with its reason: listed failed, saying why, and not a service that
/// merely stopped. Started, the reason goes.
#[test]
fn a_restore_that_did_not_start_says_why() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "smtp", "name": "mail"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "mail"})));
    w.server.stop().unwrap();
    w.guest.lock().unwrap().missing_program = Some("mailpit");
    w.server.start().unwrap();

    let listed = ok(act(&w.server, "service.list", Data::new()));
    let mail = &listed["services"][0];
    assert_eq!(mail["state"], "failed");
    assert_eq!(mail["problem"]["code"], "server.program_missing");
    assert_eq!(mail["problem"]["values"]["program"], "mailpit");

    w.guest.lock().unwrap().missing_program = None;
    ok(act(&w.server, "service.start", data!({"service": "mail"})));
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["state"], "running");
    assert!(listed["services"][0]["problem"].is_null());
}

/// What the Server needs from its machine and network, asked of them only
/// when a page asks: their state, and which programs the machine has --
/// every built-in's `requires` unless told which.
#[test]
fn the_check_says_what_the_machine_has_for_every_kind() {
    let w = world();
    w.guest.lock().unwrap().has = vec!["python3", "nginx", "bash"];
    let checked = ok(act(&w.server, "server.check", Data::new()));
    assert_eq!(checked["host"]["state"], "running");
    assert_eq!(checked["host"]["agent"], true);
    assert!(checked["network"].is_null());
    let programs = &checked["host"]["programs"];
    assert_eq!(programs["python3"], true);
    assert_eq!(programs["nginx"], true);
    assert_eq!(programs["mailpit"], false);
    assert_eq!(programs["psql"], false);

    let one = ok(act(
        &w.server,
        "server.check",
        data!({"programs": ["/srv/eco.py", "python3"]}),
    ));
    assert_eq!(
        one["host"]["programs"],
        json!({"/srv/eco.py": false, "python3": true})
    );

    let catalog = ok(act(&w.server, "service.catalog", Data::new()));
    let smtp = catalog["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "smtp")
        .unwrap()
        .clone();
    assert_eq!(smtp["requires"], json!(["mailpit", "bash"]));

    // A machine that cannot be asked: no agent, and nothing said about programs.
    w.guest.lock().unwrap().unavailable = true;
    let off = ok(act(&w.server, "server.check", Data::new()));
    assert_eq!(off["host"]["agent"], false);
    assert_eq!(off["host"]["programs"], json!({}));
}

#[test]
fn https_at_port_zero_is_off_and_once_on_is_an_endpoint_that_claims_its_port() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "plain", "config": {"port": 8080}}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "plain"})));
    let listed = ok(act(&w.server, "service.list", Data::new()));
    let endpoints = listed["services"][0]["endpoints"].as_array().unwrap();
    assert_eq!(endpoints.len(), 1, "{endpoints:?}");
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "nginx", "name": "secure",
               "config": {"mode": "proxy", "port": 8081, "https_port": 8443}}),
    ));
    ok(act(
        &w.server,
        "service.start",
        data!({"service": "secure"}),
    ));
    let listed = ok(act(&w.server, "service.list", data!({})));
    let secure = &listed["services"][1];
    assert_eq!(secure["endpoints"][1]["protocol"], "https");
    assert_eq!(secure["endpoints"][1]["port"], 8443);
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "clash", "config": {"port": 8443}}),
    ));
    let refused = act(&w.server, "service.start", data!({"service": "clash"}));
    assert_eq!(refused.reason.unwrap().code, "server.port_taken");
}

#[test]
fn a_service_is_made_started_listed_read_and_stopped() {
    let w = world();
    let made = ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site", "config": {"port": 8080}}),
    ));
    assert_eq!(made["state"], "created");

    let started = ok(act(&w.server, "service.start", data!({"service": "site"})));
    assert_eq!(started["state"], "running");
    let pid = started["pid"].as_i64().unwrap();

    // Page contents arrive as a literal argument, never as Python source.
    let id = format!("{}-site", w.server.id());
    assert!(
        w.guest
            .lock()
            .unwrap()
            .files
            .contains_key(&format!("/etc/beatbox/{id}/web-run.py"))
    );
    assert_eq!(
        last_sh(&w.guest, START),
        [
            LOG_DIR,
            &id,
            "/",
            "python3",
            &format!("/etc/beatbox/{id}/web-run.py"),
            "serve",
            "page",
            "/srv/www",
            "<h1>It works</h1>",
            "8080",
            "",
            "/",
            &format!("/var/lib/beatbox/{id}/page"),
            "0",
            &format!("/etc/beatbox/{id}/tls")
        ]
    );

    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["state"], "running");
    assert_eq!(listed["services"][0]["pid"], pid);

    let logs = ok(act(
        &w.server,
        "service.logs",
        data!({"service": "site", "lines": 2}),
    ));
    assert_eq!(logs["lines"][1], "GET / 200");

    let stopped = ok(act(&w.server, "service.stop", data!({"service": "site"})));
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(last_sh(&w.guest, STOP), [pid.to_string()]);
    assert!(w.guest.lock().unwrap().running.is_empty());

    assert_eq!(
        w.log.names(),
        [
            "service.created",
            "service.started",
            "service.health.changed",
            "service.stopped",
            "service.health.changed"
        ]
    );
}

#[test]
fn a_configured_value_is_one_argument_never_a_command() {
    let w = world();
    let definition = json!({
        "id": "app", "version": "1", "name": "App",
        "runtime": {"type": "process", "command": ["/srv/app", "--mode={mode}"]},
        "configuration": {"mode": {"kind": "text"}}
    });
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": definition, "name": "app", "config": {"mode": "x; reboot"}}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "app"})));
    let parameters = last_sh(&w.guest, START);
    assert_eq!(parameters[3..], ["/srv/app", "--mode=x; reboot"]);
    // And the script is the crate's own, word for word.
    let guest = w.guest.lock().unwrap();
    assert!(
        guest
            .calls
            .iter()
            .all(|(cap, args)| cap != "process.execute"
                || args["command"] != "/bin/sh"
                || [START, ALIVE, STOP, PROGRAM_CHECK]
                    .contains(&words(&args["arguments"])[1].as_str()))
    );
}

#[test]
fn a_service_that_dies_on_its_own_is_found_and_said() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.guest.lock().unwrap().running.clear();
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["state"], "failed");
    assert!(w.log.names().contains(&"service.failed".to_owned()));
}

#[test]
fn a_pid_handed_to_something_else_is_not_the_service() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    let pid = ok(act(&w.server, "service.start", data!({"service": "site"})))["pid"]
        .as_i64()
        .unwrap();
    w.guest.lock().unwrap().running.insert(pid, "bash".into());
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["state"], "failed");
    // And nothing was killed on the strength of that pid.
    let stopped = ok(act(&w.server, "service.stop", data!({"service": "site"})));
    assert_eq!(stopped["state"], "failed");
    assert_eq!(w.guest.lock().unwrap().running[&pid], "bash");
}

#[test]
fn missing_email_program_is_explained_before_spawn_and_can_be_retried() {
    let w = world();
    w.guest.lock().unwrap().missing_program = Some("mailpit");
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "smtp", "name": "email"}),
    ));
    let (code, values) = no(act(&w.server, "service.start", data!({"service": "email"})));
    assert_eq!(code, "server.program_missing");
    assert_eq!(values["service"], "email");
    assert_eq!(values["program"], "mailpit");
    {
        let guest = w.guest.lock().unwrap();
        assert!(guest.running.is_empty());
        assert!(
            !guest
                .calls
                .iter()
                .any(|(cap, args)| cap == "process.execute"
                    && words(&args["arguments"]).get(1).map(String::as_str) == Some(START))
        );
    }
    w.guest.lock().unwrap().missing_program = None;
    let started = ok(act(&w.server, "service.start", data!({"service": "email"})));
    assert_eq!(started["state"], "running");
    assert_eq!(started["health"], "healthy");
}

#[test]
fn website_preparation_explains_missing_folder_and_allows_retry() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition":"http-static", "name":"site", "config":{"mode":"folder", "root":"/srv/existing"}}),
    ));
    w.guest.lock().unwrap().refuse_prepare =
        Some(r#"{"code":"server.web_folder_missing","values":{"folder":"/srv/existing"}}"#);
    let (code, values) = no(act(&w.server, "service.start", data!({"service":"site"})));
    assert_eq!(code, "server.web_folder_missing");
    assert_eq!(values["folder"], "/srv/existing");
    assert_eq!(values["machine"], "web");
    assert!(w.guest.lock().unwrap().running.is_empty());
    w.guest.lock().unwrap().refuse_prepare = None;
    let started = ok(act(&w.server, "service.start", data!({"service":"site"})));
    assert_eq!(started["state"], "running");
    assert!(
        !w.guest
            .lock()
            .unwrap()
            .files
            .contains_key("/srv/existing/index.html")
    );
}

#[test]
fn unstructured_preparation_failures_have_a_readable_refusal() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition":"http-static","name":"site"}),
    ));
    w.guest.lock().unwrap().refuse_prepare = Some("not JSON");
    let (code, values) = no(act(&w.server, "service.start", data!({"service":"site"})));
    assert_eq!(code, "server.prepare_failed");
    assert_eq!(values["service"], "site");
}

#[test]
fn a_service_that_will_not_start_says_what_it_said() {
    let w = world();
    w.guest.lock().unwrap().refuse_start = Some("setsid: failed to execute python3: No such file");
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    let (code, values) = no(act(&w.server, "service.start", data!({"service": "site"})));
    assert_eq!(code, "server.did_not_start");
    assert!(values["said"].contains("No such file"));
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["state"], "failed");
}

#[test]
fn two_running_services_may_not_share_a_port() {
    let w = world();
    for name in ["one", "two"] {
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": "http-static", "name": name}),
        ));
    }
    ok(act(&w.server, "service.start", data!({"service": "one"})));
    let (code, values) = no(act(&w.server, "service.start", data!({"service": "two"})));
    assert_eq!(
        (code.as_str(), values["by"].as_str()),
        ("server.port_taken", "one")
    );
    ok(act(
        &w.server,
        "service.configure",
        data!({"service": "two", "config": {"port": 8001}}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "two"})));
}

#[test]
fn what_is_asked_wrongly_is_refused_by_name() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    let cases = [
        (
            "service.create",
            data!({"definition": "http-static", "name": "site"}),
            "server.name_taken",
        ),
        (
            "service.create",
            data!({"definition": "http-static", "name": "Site!"}),
            "server.bad_name",
        ),
        (
            "service.create",
            data!({"definition": "missing-service", "name": "mail"}),
            "server.no_such_definition",
        ),
        (
            "service.create",
            data!({"definition": "dns", "name": "names"}),
            "server.needs_network",
        ),
        (
            "service.create",
            data!({"definition": "http-static", "name": "x", "config": {"prot": 1}}),
            "server.config_unknown",
        ),
        (
            "service.start",
            data!({"service": "nope"}),
            "server.no_such_service",
        ),
        (
            "service.create",
            data!({"name": "x"}),
            "engine.missing_argument",
        ),
    ];
    for (capability, arguments, expected) in cases {
        let (code, _) = no(act(&w.server, capability, arguments));
        assert_eq!(code, expected, "{capability}");
    }
}

#[test]
fn a_server_without_a_host_cannot_hold_a_process_service() {
    let guest: Shared = Arc::default();
    let runtime = runtime(&guest);
    let room = room();
    let server = runtime
        .create(
            SandboxConfig::new("server")
                .unwrap()
                .with_settings(data!({"workspace": room.to_string_lossy()})),
        )
        .unwrap();
    let (code, _) = no(act(
        &server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    assert_eq!(code, "server.needs_host");
    let _ = std::fs::remove_dir_all(room);
}

#[test]
fn a_windows_guest_is_refused_for_now() {
    let w = world_with("windows", Data::new());
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    let (code, _) = no(act(&w.server, "service.start", data!({"service": "site"})));
    assert_eq!(code, "server.guest_not_linux");
}

#[test]
fn configuring_waits_for_a_stop_and_never_says_a_secret() {
    let w = world();
    let definition = json!({
        "id": "billing", "version": "1", "name": "Billing",
        "runtime": {"type": "process", "command": ["/srv/billing"]},
        "configuration": {"api_key": {"kind": "secret"}, "mode": {"kind": "text", "default": "test"}}
    });
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": definition, "name": "billing", "config": {"api_key": "hunter2"}}),
    ));
    ok(act(
        &w.server,
        "service.start",
        data!({"service": "billing"}),
    ));
    let (code, _) = no(act(
        &w.server,
        "service.configure",
        data!({"service": "billing", "config": {"api_key": "x"}}),
    ));
    assert_eq!(code, "server.configure_while_running");
    ok(act(
        &w.server,
        "service.stop",
        data!({"service": "billing"}),
    ));
    let configured = ok(act(
        &w.server,
        "service.configure",
        data!({"service": "billing", "config": {"api_key": "swordfish"}}),
    ));
    assert_eq!(configured["config"]["api_key"], "••••");
    let configured = ok(act(
        &w.server,
        "service.configure",
        data!({"service": "billing", "config": {"mode": "live"}}),
    ));
    assert_eq!(configured["config"]["mode"], "live");
    assert_eq!(configured["config"]["api_key"], "••••");
    let kept: Value =
        serde_json::from_str(&std::fs::read_to_string(w.room.join("services.json")).unwrap())
            .unwrap();
    assert!(
        kept.to_string().contains("swordfish"),
        "the omitted secret was preserved"
    );
    let listed = ok(act(&w.server, "service.list", Data::new()));
    let everything_said = format!("{:?} {:?}", listed, w.log.events());
    assert!(!everything_said.contains("hunter2") && !everything_said.contains("swordfish"));
}

#[test]
fn the_services_are_kept_privately_and_come_back() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    assert!(w.room.join("services.json").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = w.room.join("services.json");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Stopping the Server stops its services and keeps them meant to run;
    // starting it again starts them again.
    w.server.stop().unwrap();
    assert!(w.guest.lock().unwrap().running.is_empty());
    w.server.start().unwrap();
    assert_eq!(w.guest.lock().unwrap().running.len(), 1);

    // A daemon that restarts finds them where they were left.
    let again = runtime(&w.guest);
    let host = again
        .restore(
            w.host.identity(),
            w.host.config(),
            beatbox_engine::Lifecycle::Running,
            None,
        )
        .unwrap();
    let server = again
        .restore(
            w.server.identity(),
            w.server.config(),
            beatbox_engine::Lifecycle::Running,
            None,
        )
        .unwrap();
    let listed = ok(act(&server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["service"], "site");
    assert_eq!(listed["services"][0]["state"], "running");
    drop(host);
}

#[test]
fn editing_a_stopped_website_applies_its_new_page_and_port_on_start() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site", "config": {"root": "/srv/custom"}}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    ok(act(&w.server, "service.stop", data!({"service": "site"})));
    let edited = ok(act(
        &w.server,
        "service.configure",
        data!({"service": "site", "config": {"port": 9090, "page": "<h1>Updated</h1>"}}),
    ));
    assert_eq!(edited["config"]["root"], "/srv/custom");
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"].as_array().unwrap().len(), 1);
    assert_eq!(listed["services"][0]["state"], "stopped");
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    assert_eq!(
        last_sh(&w.guest, START)[7..10],
        ["/srv/custom", "<h1>Updated</h1>", "9090"]
    );
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["endpoints"][0]["port"], 9090);
}

#[test]
fn reset_forgets_every_service() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.server.reset().unwrap();
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"], json!([]));
    assert!(w.guest.lock().unwrap().running.is_empty());
}

#[test]
fn a_host_destroyed_under_it_does_not_keep_the_server_from_stopping() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.host.destroy().unwrap();
    w.server.stop().unwrap();
    let (code, _) = no(act(&w.server, "service.start", data!({"service": "site"})));
    assert_eq!(code, "server.host_gone");
}

#[test]
fn the_library_adds_definitions_and_says_which_it_refused() {
    let library = room();
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(
        library.join("echo.json"),
        json!({
            "id": "echo", "version": "1", "name": "Echo",
            "runtime": {"type": "process", "command": ["/srv/echo", "{port}"]},
            "configuration": {"port": {"kind": "number", "default": 3610}},
            "interfaces": [{"transport": "tcp", "port": "{port}"}]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(library.join("broken.json"), "{\"id\": \"broken\"}").unwrap();
    let w = world_with("linux", data!({"definitions": library.to_string_lossy()}));
    let catalog = ok(act(&w.server, "service.catalog", Data::new()));
    let sources: BTreeMap<String, String> = catalog["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["id"].as_str().unwrap().to_owned(),
                d["source"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(sources["echo"], "library");
    assert_eq!(sources["dns"], "builtin");
    assert_eq!(
        catalog["refused"][0]["reason"]["code"],
        "server.bad_definition"
    );
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "echo", "name": "echo"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "echo"})));
    assert_eq!(last_sh(&w.guest, START)[3..], ["/srv/echo", "3610"]);
    let _ = std::fs::remove_dir_all(library);
}

/// A definition from a folder is trusted with root in the guest, so: it may
/// not pass for one BeatBox brings, what it runs can be read before it is
/// made, making it runs nothing, and a file changed afterwards does not
/// change what was made from it.
#[test]
fn a_library_definition_is_read_before_it_runs_and_cannot_pass_for_a_built_in() {
    let library = room();
    std::fs::create_dir_all(&library).unwrap();
    let echo = |program: &str| {
        json!({
            "id": "echo", "version": "1", "name": "Echo",
            "runtime": {"type": "process", "command": [program, "{port}"]},
            "configuration": {"port": {"kind": "number", "default": 3610}},
            "files": [{"path": "/etc/echo/{_id}.conf", "content": "x"}],
            "interfaces": [{"transport": "tcp", "port": "{port}"}]
        })
        .to_string()
    };
    std::fs::write(library.join("echo.json"), echo("/srv/echo")).unwrap();
    std::fs::write(
        library.join("postgresql.json"),
        json!({
            "id": "postgresql", "version": "9", "name": "PostgreSQL",
            "runtime": {"type": "process", "command": ["/srv/anything"]}
        })
        .to_string(),
    )
    .unwrap();
    let w = world_with("linux", data!({"definitions": library.to_string_lossy()}));
    let catalog = ok(act(&w.server, "service.catalog", Data::new()));
    let find = |id: &str| {
        catalog["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == id)
            .cloned()
            .unwrap()
    };
    assert_eq!(find("postgresql")["source"], "builtin");
    assert_eq!(find("postgresql")["version"], "5");
    let refused = &catalog["refused"][0];
    assert!(
        refused["file"]
            .as_str()
            .unwrap()
            .ends_with("postgresql.json")
    );
    assert_eq!(refused["reason"]["code"], "server.define_builtin");
    let runs = &find("echo")["runs"];
    assert_eq!(runs["command"], json!(["/srv/echo", "{port}"]));
    assert_eq!(runs["files"], json!(["/etc/echo/{_id}.conf"]));

    let before = w.guest.lock().unwrap().calls.len();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "echo", "name": "echo"}),
    ));
    assert_eq!(w.guest.lock().unwrap().calls.len(), before, "made, not run");

    std::fs::write(library.join("echo.json"), echo("/srv/other")).unwrap();
    ok(act(&w.server, "service.start", data!({"service": "echo"})));
    assert_eq!(last_sh(&w.guest, START)[3], "/srv/echo");
    let _ = std::fs::remove_dir_all(library);
}

mod health {
    use super::*;

    fn site(w: &World) -> Data {
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": "http-static", "name": "site", "config": {"port": 8080}}),
        ));
        ok(act(&w.server, "service.start", data!({"service": "site"})))
    }

    fn changes(w: &World) -> Vec<(String, String)> {
        w.log
            .events()
            .iter()
            .filter(|e| e.name() == "service.health.changed")
            .map(|e| {
                (
                    e.data()["from"].as_str().unwrap().to_owned(),
                    e.data()["to"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn pair(from: &str, to: &str) -> (String, String) {
        (from.to_owned(), to.to_owned())
    }

    #[test]
    fn a_service_that_answers_is_healthy_as_soon_as_it_starts() {
        let w = world();
        let started = site(&w);
        assert_eq!(started["health"], "healthy");
        let checks = w.guest.lock().unwrap().checks.clone();
        assert_eq!(checks.len(), 1, "healthy at the first look: no second");
        assert_eq!(checks[0][4..], ["8080", "/"]);
        assert_eq!(changes(&w), [pair("unknown", "healthy")]);
    }

    #[test]
    fn one_that_never_answers_is_running_and_said_to_be_unhealthy() {
        let w = world();
        w.guest.lock().unwrap().sick = true;
        let started = site(&w);
        assert_eq!(started["state"], "running", "slow is said, not killed");
        assert_eq!(started["health"], "unhealthy");
        assert!(
            w.guest.lock().unwrap().checks.len() > 1,
            "it was given its start period"
        );
        assert_eq!(changes(&w), [pair("unknown", "unhealthy")]);
    }

    #[test]
    fn asking_checks_now_and_says_only_changes() {
        let w = world();
        site(&w);
        w.guest.lock().unwrap().sick = true;
        let now = ok(act(&w.server, "service.health", data!({"service": "site"})));
        assert_eq!(now["health"], "unhealthy");
        ok(act(&w.server, "service.health", data!({"service": "site"})));
        w.guest.lock().unwrap().sick = false;
        let now = ok(act(&w.server, "service.health", data!({"service": "site"})));
        assert_eq!(now["health"], "healthy");
        ok(act(&w.server, "service.stop", data!({"service": "site"})));
        assert_eq!(
            changes(&w),
            [
                pair("unknown", "healthy"),
                pair("healthy", "unhealthy"),
                pair("unhealthy", "healthy"),
                pair("healthy", "unknown"),
            ]
        );
    }

    #[test]
    fn a_service_with_no_check_stays_unknown_and_nothing_is_run() {
        let w = world();
        let definition = json!({
            "id": "quiet", "version": "1", "name": "Quiet",
            "runtime": {"type": "process", "command": ["/srv/quiet"]}
        });
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": definition, "name": "quiet"}),
        ));
        let started = ok(act(&w.server, "service.start", data!({"service": "quiet"})));
        assert_eq!(started["health"], "unknown");
        assert!(w.guest.lock().unwrap().checks.is_empty());
    }

    #[test]
    fn tcp_and_command_checks_are_argv_too() {
        let w = world();
        let definition = json!({
            "id": "erp", "version": "1", "name": "ERP",
            "runtime": {"type": "process", "command": ["/srv/erp", "{port}"]},
            "configuration": {"port": {"kind": "number", "default": 3610}},
            "interfaces": [{"transport": "tcp", "port": "{port}"}],
            "health": {"type": "command", "command": ["/srv/erp-ping", "--port={port}"]}
        });
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": definition.clone(), "name": "erp"}),
        ));
        ok(act(&w.server, "service.start", data!({"service": "erp"})));
        let mut tcp = definition;
        tcp["id"] = json!("erp-tcp");
        tcp["health"] = json!({"type": "tcp", "port": "{port}"});
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": tcp, "name": "erp2", "config": {"port": 3611}}),
        ));
        ok(act(&w.server, "service.start", data!({"service": "erp2"})));
        let checks = w.guest.lock().unwrap().checks.clone();
        assert_eq!(checks[0], ["/srv/erp-ping", "--port=3610"]);
        assert_eq!(checks[1][..3], ["bash", "-c", TCP_CHECK]);
        assert_eq!(checks[1][4..], ["3611"]);
    }

    #[test]
    fn where_it_answers_is_in_the_list_and_the_facts_while_it_runs() {
        let w = world();
        site(&w);
        let expected = json!([{
            "service": "site", "protocol": "http", "transport": "tcp",
            "address": "10.10.10.10", "port": 8080
        }]);
        let listed = ok(act(&w.server, "service.list", Data::new()));
        assert_eq!(listed["services"][0]["endpoints"], expected);
        assert_eq!(w.server.refresh().unwrap().facts["endpoints"], expected);
        ok(act(&w.server, "service.stop", data!({"service": "site"})));
        assert_eq!(w.server.refresh().unwrap().facts["endpoints"], json!([]));
    }
}

mod capabilities {
    use super::*;

    fn billing(w: &World) {
        let definition = json!({
            "id": "billing", "version": "1", "name": "Billing",
            "runtime": {"type": "process", "command": ["/srv/billing"]},
            "configuration": {"api_key": {"kind": "secret"}},
            "capabilities": [{
                "capability": "billing.invoice.create",
                "inputs": [
                    {"name": "amount", "kind": "number"},
                    {"name": "memo", "kind": "text", "required": false}
                ],
                "outputs": [{"name": "exit_code", "kind": "number"}, {"name": "stdout", "kind": "text"}],
                "run": ["/srv/billing-cli", "invoice", "{amount}", "--memo={memo}", "--key", "{api_key}"]
            }]
        });
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": definition, "name": "billing", "config": {"api_key": "hunter2"}}),
        ));
    }

    fn call(w: &World, capability: &str, arguments: Value) -> ActionOutcome {
        act(
            &w.server,
            "service.call",
            data!({"service": "billing", "capability": capability, "arguments": arguments}),
        )
    }

    #[test]
    fn describe_says_what_it_can_do_and_never_its_secrets() {
        let w = world();
        billing(&w);
        let described = ok(act(
            &w.server,
            "service.describe",
            data!({"service": "billing"}),
        ));
        assert_eq!(
            described["capabilities"][0]["capability"],
            "billing.invoice.create"
        );
        assert_eq!(
            described["capabilities"][0]["inputs"][0],
            json!({"name": "amount", "kind": "number", "required": true})
        );
        assert_eq!(described["config"]["api_key"], "••••");
        assert!(
            !described
                .values()
                .any(|v| v.to_string().contains("hunter2"))
        );
    }

    #[test]
    fn a_call_runs_its_argv_with_the_inputs_and_the_configuration_in_it() {
        let w = world();
        billing(&w);
        let answer = ok(call(
            &w,
            "billing.invoice.create",
            json!({"amount": 10, "memo": "x; reboot"}),
        ));
        assert_eq!(answer["exit_code"], 0);
        // What it printed back is masked; the argv it ran is whole.
        assert_eq!(
            answer["stdout"],
            "done invoice 10 --memo=x; reboot --key ••••\n"
        );
        let guest = w.guest.lock().unwrap();
        let (_, last) = guest.calls.last().unwrap();
        assert_eq!(last["command"], "/srv/billing-cli");
        assert_eq!(
            words(&last["arguments"]),
            ["invoice", "10", "--memo=x; reboot", "--key", "hunter2"]
        );
    }

    #[test]
    fn an_optional_input_left_out_is_empty_not_its_placeholder() {
        let w = world();
        billing(&w);
        ok(call(&w, "billing.invoice.create", json!({"amount": 10})));
        let guest = w.guest.lock().unwrap();
        let (_, last) = guest.calls.last().unwrap();
        assert_eq!(
            words(&last["arguments"]),
            ["invoice", "10", "--memo=", "--key", "hunter2"]
        );
    }

    #[test]
    fn its_inputs_are_checked_as_any_actions_are() {
        let w = world();
        billing(&w);
        let cases = [
            (
                "billing.invoice.create",
                json!({}),
                "engine.missing_argument",
            ),
            (
                "billing.invoice.create",
                json!({"amount": "ten"}),
                "engine.bad_argument",
            ),
            ("billing.refund", json!({}), "server.no_such_capability"),
        ];
        for (capability, arguments, expected) in cases {
            let (code, _) = no(call(&w, capability, arguments));
            assert_eq!(code, expected, "{capability}");
        }
        let calls_before = w.guest.lock().unwrap().calls.len();
        assert_eq!(calls_before, 0, "nothing refused reached the guest");
        let (code, _) = no(act(
            &w.server,
            "service.call",
            data!({"service": "nope", "capability": "billing.refund"}),
        ));
        assert_eq!(code, "server.no_such_service");
    }
}

#[test]
fn a_machine_that_is_off_is_named_as_the_reason() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    w.host.stop().unwrap();
    let (code, _) = no(act(&w.server, "service.start", data!({"service": "site"})));
    assert_eq!(code, "server.host_not_running");
    assert!(
        w.guest.lock().unwrap().calls.is_empty(),
        "nothing was asked of a machine that is off"
    );
}

#[test]
fn a_host_refusal_does_not_claim_the_running_server_is_off() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    w.guest.lock().unwrap().unavailable = true;
    let (code, _) = no(act(&w.server, "service.start", data!({"service": "site"})));
    assert_eq!(code, "server.host_unavailable");
    assert_eq!(w.server.lifecycle(), beatbox_engine::Lifecycle::Running);
}

#[test]
fn listing_with_the_machine_off_shows_the_services_stopped_not_an_error() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.guest.lock().unwrap().unavailable = true;
    let listed = ok(act(
        &w.server,
        "service.list",
        data!({"check_health": true}),
    ));
    let site = &listed["services"][0];
    assert_eq!(site["service"], "site");
    assert_eq!(site["state"], "stopped", "the machine took it with it");
    assert_eq!(site["health"], "unknown");
    // Still meant to run: a start of the Server brings it back.
    w.guest.lock().unwrap().unavailable = false;
    w.server.stop().unwrap();
    w.server.start().unwrap();
    let listed = ok(act(&w.server, "service.list", data!({})));
    assert_eq!(listed["services"][0]["state"], "running");
}

/// A machine whose state was saved comes back with its processes in it:
/// the same one is found running again, not a second started beside it.
#[test]
fn a_machine_back_from_a_saved_state_still_runs_its_services() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    let started = ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.guest.lock().unwrap().unavailable = true;
    let off = ok(act(
        &w.server,
        "service.list",
        data!({"check_health": true}),
    ));
    assert_eq!(off["services"][0]["state"], "stopped");

    w.guest.lock().unwrap().unavailable = false;
    let back = ok(act(
        &w.server,
        "service.list",
        data!({"check_health": true}),
    ));
    assert_eq!(back["services"][0]["state"], "running");
    assert_eq!(back["services"][0]["pid"], started["pid"]);
    w.server.stop().unwrap();
    w.server.start().unwrap();
    let starts = w
        .guest
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(cap, args)| {
            cap == "process.execute"
                && words(&args["arguments"]).get(1).map(String::as_str) == Some(START)
        })
        .count();
    assert_eq!(
        starts, 2,
        "one at first, one after the Server's own stop: none on coming back"
    );
}

/// Booted afresh instead, the old pid is nobody's: stopped, not failed.
#[test]
fn a_machine_booted_afresh_leaves_its_services_stopped_not_failed() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.guest.lock().unwrap().unavailable = true;
    ok(act(
        &w.server,
        "service.list",
        data!({"check_health": true}),
    ));
    {
        let mut guest = w.guest.lock().unwrap();
        guest.unavailable = false;
        guest.running.clear();
    }
    let back = ok(act(
        &w.server,
        "service.list",
        data!({"check_health": true}),
    ));
    assert_eq!(back["services"][0]["state"], "stopped");
    // Still meant to run: the page says it stopped with its machine.
    assert_eq!(back["services"][0]["wanted"], true);
    assert!(w.log.matching("service.failed").is_empty());
}

/// Switched off and on while nobody asked the Server: it never saw the
/// machine off, but the machine started since, so not a death.
#[test]
fn a_machine_restarted_unseen_leaves_its_services_stopped_not_failed() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.host.stop().unwrap();
    w.guest.lock().unwrap().running.clear();
    w.host.start().unwrap();
    let back = ok(act(&w.server, "service.list", data!({})));
    assert_eq!(back["services"][0]["state"], "stopped");
    assert_eq!(back["services"][0]["wanted"], true);
    assert!(w.log.matching("service.failed").is_empty());
}

/// Gone back to a save point from before the service: the machine never
/// stopped, and the process is gone with the rest of its present.
#[test]
fn a_machine_gone_back_to_before_a_service_leaves_it_stopped_not_failed() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    {
        let mut guest = w.guest.lock().unwrap();
        guest.running.clear();
        guest.went_back_at = Some(1_700_000_000_000);
    }
    w.host.refresh().unwrap();
    let back = ok(act(&w.server, "service.list", data!({})));
    assert_eq!(back["services"][0]["state"], "stopped");
    assert!(w.log.matching("service.failed").is_empty());
    // Started again in this run, a death is a death.
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.guest.lock().unwrap().running.clear();
    let died = ok(act(&w.server, "service.list", data!({})));
    assert_eq!(died["services"][0]["state"], "failed");
}

#[test]
fn starting_the_server_discovers_the_web_address_without_refreshing_the_host() {
    let w = world();
    w.guest.lock().unwrap().addresses = Some(vec!["127.0.0.1".into()]);
    w.host.refresh().unwrap();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site", "config": {"port": 80}}),
    ));
    w.guest.lock().unwrap().addresses = Some(vec!["127.0.0.1".into(), "10.50.20.11".into()]);
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.server.stop().unwrap();
    w.guest.lock().unwrap().addresses = Some(vec!["10.50.20.15".into()]);
    w.server.start().unwrap();
    assert_eq!(
        w.server.state().fact("endpoints").unwrap()[0]["address"],
        "10.50.20.15"
    );
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(
        listed["services"][0]["endpoints"][0]["address"],
        "10.50.20.15"
    );
    assert_eq!(listed["services"][0]["endpoints"][0]["port"], 80);
    // A subsequent list also picks up changes without an unrelated Host refresh.
    w.guest.lock().unwrap().addresses = Some(vec!["10.50.20.16".into()]);
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(
        listed["services"][0]["endpoints"][0]["address"],
        "10.50.20.16"
    );
    let before = w.guest.lock().unwrap().calls.len();
    w.server.refresh().unwrap();
    assert_eq!(
        w.guest.lock().unwrap().calls.len(),
        before,
        "observe must not ask the guest"
    );
}

#[test]
fn service_addresses_skip_loopback_and_prefer_the_guest_ipv4() {
    let w = world();
    w.guest.lock().unwrap().addresses = Some(
        [
            "127.0.0.1",
            "::1",
            "0.0.0.0",
            "fe80::1",
            "fd00::10",
            "10.10.10.10",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(
        listed["services"][0]["endpoints"][0]["address"],
        "10.10.10.10"
    );

    w.guest.lock().unwrap().addresses = Some(vec!["127.0.0.1".into(), "::1".into()]);
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["endpoints"], json!([]));
    assert_eq!(listed["services"][0]["state"], "running");

    w.guest.lock().unwrap().addresses = Some(vec!["::1".into(), "fd00::10".into()]);
    let listed = ok(act(&w.server, "service.list", Data::new()));
    assert_eq!(listed["services"][0]["endpoints"][0]["address"], "fd00::10");
}

mod own {
    use super::*;

    #[test]
    fn settings_are_read_changed_kept_and_bounded() {
        let w = world();
        let said = ok(act(&w.server, "server.settings", Data::new()));
        assert_eq!(said["restore"], true);
        assert_eq!(
            said["start_period_s"], 0.3,
            "the configuration's, until changed"
        );
        let said = ok(act(
            &w.server,
            "server.settings",
            data!({"start_period_s": 30, "restore": false}),
        ));
        assert_eq!(said["start_period_s"], 30.0);
        assert_eq!(said["restore"], false);
        let (code, _) = no(act(
            &w.server,
            "server.settings",
            data!({"start_period_s": 0}),
        ));
        assert_eq!(code, "server.bad_period");
        assert!(w.room.join("own.json").is_file());
    }

    #[test]
    fn without_restore_starting_the_server_starts_nothing() {
        let w = world();
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": "http-static", "name": "site"}),
        ));
        ok(act(&w.server, "service.start", data!({"service": "site"})));
        ok(act(&w.server, "server.settings", data!({"restore": false})));
        w.server.stop().unwrap();
        w.server.start().unwrap();
        assert!(w.guest.lock().unwrap().running.is_empty());
        ok(act(&w.server, "server.settings", data!({"restore": true})));
        w.server.stop().unwrap();
        w.server.start().unwrap();
        assert_eq!(
            w.guest.lock().unwrap().running.len(),
            1,
            "still meant to run"
        );
    }

    fn echo() -> Value {
        json!({
            "id": "eco", "version": "1", "name": "Eco",
            "runtime": {"type": "process", "command": ["python3", "/srv/eco.py", "{port}"]},
            "configuration": {"port": {"kind": "number", "default": 3610}},
            "interfaces": [{"transport": "tcp", "port": "{port}"}],
            "health": {"type": "tcp", "port": "{port}"}
        })
    }

    #[test]
    fn a_definition_made_here_is_written_to_the_library_and_offered() {
        let library = room();
        let w = world_with("linux", data!({"definitions": library.to_string_lossy()}));
        let said = ok(act(
            &w.server,
            "service.define",
            data!({"definition": echo()}),
        ));
        assert_eq!(said["id"], "eco");
        assert!(library.join("eco.json").is_file());
        let catalog = ok(act(&w.server, "service.catalog", Data::new()));
        assert!(
            catalog["definitions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["id"] == "eco" && d["source"] == "library")
        );

        let (code, _) = no(act(
            &w.server,
            "service.define",
            data!({"definition": echo()}),
        ));
        assert_eq!(code, "server.define_exists");
        ok(act(
            &w.server,
            "service.define",
            data!({"definition": echo(), "replace": true}),
        ));
        let mut builtin = echo();
        builtin["id"] = json!("dns");
        let (code, _) = no(act(
            &w.server,
            "service.define",
            data!({"definition": builtin}),
        ));
        assert_eq!(code, "server.define_builtin");
        let mut broken = echo();
        broken["runtime"]["command"] = json!([]);
        let (code, _) = no(act(
            &w.server,
            "service.define",
            data!({"definition": broken}),
        ));
        assert_eq!(
            code, "server.bad_definition",
            "checked before anything is written"
        );
        let _ = std::fs::remove_dir_all(library);
    }

    #[test]
    fn without_a_library_nothing_is_written() {
        let w = world();
        let (code, _) = no(act(
            &w.server,
            "service.define",
            data!({"definition": echo()}),
        ));
        assert_eq!(code, "server.no_library");
    }
}

/// A Host deleted under a running Server (#70): its services went with it.
/// The Server still lists them -- stopped, health unknown, no address --
/// lets them be removed, and says what to do when asked to run one.
#[test]
fn a_host_deleted_under_running_services_leaves_them_stopped_and_removable() {
    let w = world();
    ok(act(
        &w.server,
        "service.create",
        data!({"definition": "http-static", "name": "site"}),
    ));
    ok(act(&w.server, "service.start", data!({"service": "site"})));
    w.host.destroy().unwrap();

    let listed = ok(act(
        &w.server,
        "service.list",
        data!({"check_health": true}),
    ));
    let site = &listed["services"][0];
    assert_eq!(site["state"], "stopped");
    assert_eq!(site["health"], "unknown");
    assert_eq!(site["endpoints"], json!([]));
    let (code, _) = no(act(&w.server, "service.start", data!({"service": "site"})));
    assert_eq!(code, "server.host_gone");
    let (code, _) = no(act(&w.server, "service.logs", data!({"service": "site"})));
    assert_eq!(code, "server.host_gone");
    assert!(w.log.matching("service.failed").is_empty());

    ok(act(&w.server, "service.remove", data!({"service": "site"})));
    w.server.stop().unwrap();
}

/// What a service needs, how they start, and what the Server does when one
/// stops on its own.
mod dependencies {
    use super::*;

    /// The services started, in order, by their names on this Server.
    fn started(w: &World) -> Vec<String> {
        let prefix = format!("{}-", w.server.id());
        w.guest
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(capability, _)| capability == "process.execute")
            .map(|(_, arguments)| words(&arguments["arguments"]))
            .filter(|argv| argv.get(1).map(String::as_str) == Some(START))
            .map(|argv| argv[4].trim_start_matches(&prefix).to_owned())
            .collect()
    }

    fn site(w: &World, name: &str, port: u16, after: Value) {
        ok(act(
            &w.server,
            "service.create",
            data!({"definition": "http-static", "name": name, "config": {"port": port}, "after": after}),
        ));
    }

    #[test]
    fn a_service_starts_the_ones_it_needs_first() {
        let w = world();
        site(&w, "db", 8081, json!([]));
        site(&w, "app", 8082, json!(["db"]));
        let said = ok(act(&w.server, "service.start", data!({"service": "app"})));
        assert_eq!(said["state"], "running");
        assert_eq!(started(&w), ["db", "app"]);
        let listed = ok(act(&w.server, "service.list", Data::new()));
        assert_eq!(listed["services"][1]["after"], json!(["db"]));
    }

    #[test]
    fn what_a_service_needs_is_checked() {
        let w = world();
        site(&w, "db", 8081, json!([]));
        site(&w, "app", 8082, json!(["db"]));
        let (code, _) = no(act(
            &w.server,
            "service.depend",
            data!({"service": "app", "after": ["nope"]}),
        ));
        assert_eq!(code, "server.after_unknown");
        let (code, _) = no(act(
            &w.server,
            "service.depend",
            data!({"service": "app", "after": ["app"]}),
        ));
        assert_eq!(code, "server.after_itself");
        let (code, _) = no(act(
            &w.server,
            "service.depend",
            data!({"service": "db", "after": ["app"]}),
        ));
        assert_eq!(code, "server.after_loop");
        let (code, values) = no(act(&w.server, "service.remove", data!({"service": "db"})));
        assert_eq!(code, "server.still_needed");
        assert_eq!(values["by"], "app");
        // Taken back, it can go.
        ok(act(
            &w.server,
            "service.depend",
            data!({"service": "app", "after": []}),
        ));
        ok(act(&w.server, "service.remove", data!({"service": "db"})));
    }

    #[test]
    fn the_server_brings_them_back_in_the_order_they_need() {
        let w = world();
        site(&w, "app", 8082, json!([]));
        site(&w, "db", 8081, json!([]));
        ok(act(
            &w.server,
            "service.depend",
            data!({"service": "app", "after": ["db"]}),
        ));
        ok(act(&w.server, "service.start", data!({"service": "app"})));
        w.server.stop().unwrap();
        w.guest.lock().unwrap().calls.clear();
        w.server.start().unwrap();
        assert_eq!(started(&w), ["db", "app"]);
    }

    #[test]
    fn a_service_that_stops_on_its_own_is_left_by_default() {
        let w = world();
        site(&w, "site", 8081, json!([]));
        ok(act(&w.server, "service.start", data!({"service": "site"})));
        w.guest.lock().unwrap().running.clear();
        let said = ok(act(&w.server, "service.supervise", Data::new()));
        assert_eq!(said["restarted"], json!([]));
        let settings = ok(act(&w.server, "server.settings", Data::new()));
        assert_eq!(settings["on_exit"], "leave");
    }

    #[test]
    fn retry_starts_it_again_three_times_in_a_row() {
        let w = world();
        ok(act(
            &w.server,
            "server.settings",
            data!({"on_exit": "retry"}),
        ));
        site(&w, "site", 8081, json!([]));
        ok(act(&w.server, "service.start", data!({"service": "site"})));
        for attempt in 1..=3 {
            w.guest.lock().unwrap().running.clear();
            let said = ok(act(&w.server, "service.supervise", Data::new()));
            assert_eq!(said["restarted"], json!(["site"]), "attempt {attempt}");
        }
        w.guest.lock().unwrap().running.clear();
        let said = ok(act(&w.server, "service.supervise", Data::new()));
        assert_eq!(said["restarted"], json!([]), "a fourth is not tried");
        let listed = ok(act(&w.server, "service.list", Data::new()));
        assert_eq!(listed["services"][0]["state"], "failed");
        assert_eq!(listed["services"][0]["restarts"], 3);
        assert_eq!(w.log.matching("service.restarting").len(), 3);
        // Started by hand, its count starts again.
        ok(act(&w.server, "service.start", data!({"service": "site"})));
        let listed = ok(act(&w.server, "service.list", Data::new()));
        assert_eq!(listed["services"][0]["restarts"], 0);
    }

    #[test]
    fn a_machine_still_booting_does_not_spend_its_tries() {
        let w = world();
        ok(act(
            &w.server,
            "server.settings",
            data!({"on_exit": "retry"}),
        ));
        site(&w, "site", 8081, json!([]));
        w.guest.lock().unwrap().no_agent = true;
        let (code, _) = no(act(&w.server, "service.start", data!({"service": "site"})));
        assert_eq!(code, "host.no_agent");
        for _ in 0..4 {
            ok(act(&w.server, "service.supervise", Data::new()));
        }
        w.guest.lock().unwrap().no_agent = false;
        let said = ok(act(&w.server, "service.supervise", Data::new()));
        assert_eq!(said["restarted"], json!(["site"]));
        let listed = ok(act(&w.server, "service.list", Data::new()));
        assert_eq!(listed["services"][0]["restarts"], 1);
    }

    /// One that dies says so on the page, not only in the history; and
    /// "always" brings it back, while a start that was refused -- here a
    /// program that is not there -- waits for the person instead of being
    /// tried again every round.
    #[test]
    fn always_brings_back_what_died_and_leaves_what_was_refused() {
        let w = world();
        ok(act(
            &w.server,
            "server.settings",
            data!({"on_exit": "always"}),
        ));
        site(&w, "site", 8081, json!([]));
        site(&w, "other", 8082, json!([]));
        ok(act(&w.server, "service.start", data!({"service": "site"})));
        w.guest.lock().unwrap().missing_program = Some("python3");
        no(act(&w.server, "service.start", data!({"service": "other"})));
        w.guest.lock().unwrap().missing_program = None;
        w.guest.lock().unwrap().running.clear();
        let listed = ok(act(&w.server, "service.list", Data::new()));
        assert_eq!(listed["services"][0]["problem"]["code"], "server.died");
        assert_eq!(
            listed["services"][1]["problem"]["code"],
            "server.program_missing"
        );
        for _ in 0..3 {
            ok(act(&w.server, "service.supervise", Data::new()));
        }
        let listed = ok(act(&w.server, "service.list", Data::new()));
        assert_eq!(listed["services"][0]["state"], "running");
        assert!(listed["services"][0]["problem"].is_null());
        assert_eq!(listed["services"][1]["state"], "failed");
        assert_eq!(listed["services"][1]["restarts"], 0, "never tried again");
    }

    #[test]
    fn always_brings_back_what_its_machine_took_down() {
        let w = world();
        ok(act(
            &w.server,
            "server.settings",
            data!({"on_exit": "always"}),
        ));
        site(&w, "site", 8081, json!([]));
        ok(act(&w.server, "service.start", data!({"service": "site"})));
        // The machine went off and booted afresh: listed stopped, still wanted.
        w.guest.lock().unwrap().unavailable = true;
        ok(act(&w.server, "service.list", Data::new()));
        {
            let mut guest = w.guest.lock().unwrap();
            guest.unavailable = false;
            guest.running.clear();
        }
        let said = ok(act(&w.server, "service.supervise", Data::new()));
        assert_eq!(said["restarted"], json!(["site"]));
        let (code, _) = no(act(
            &w.server,
            "server.settings",
            data!({"on_exit": "sometimes"}),
        ));
        assert_eq!(code, "server.bad_on_exit");
    }

    #[test]
    fn a_service_waits_for_the_ones_it_needs_to_come_back() {
        let w = world();
        ok(act(
            &w.server,
            "server.settings",
            data!({"on_exit": "retry"}),
        ));
        site(&w, "db", 8081, json!([]));
        site(&w, "app", 8082, json!(["db"]));
        ok(act(&w.server, "service.start", data!({"service": "app"})));
        w.guest.lock().unwrap().running.clear();
        w.guest.lock().unwrap().calls.clear();
        let said = ok(act(&w.server, "service.supervise", Data::new()));
        assert_eq!(said["restarted"], json!(["db", "app"]));
        assert_eq!(started(&w), ["db", "app"]);
    }
}

/// What a Server's services share: secrets set once on the Server
/// and named by its services, and one certificate for its domain.
mod shared {
    use super::*;

    fn app(w: &World, name: &str, token: Value) -> ActionOutcome {
        act(
            &w.server,
            "service.create",
            data!({
                "name": name,
                "config": {"token": token},
                "definition": {
                    "id": "app", "version": "1", "name": "App",
                    "runtime": {"type": "process", "command": ["/srv/app", "--token={token}"]},
                    "configuration": {"token": {"kind": "secret"}}
                }
            }),
        )
    }

    /// The argv of every start, as the Host was asked, and what it concealed.
    fn starts(w: &World) -> Vec<(Vec<String>, Value)> {
        w.guest
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(capability, _)| capability == "process.execute")
            .map(|(_, arguments)| {
                let conceal = arguments.get("conceal").cloned().unwrap_or_default();
                (words(&arguments["arguments"]), conceal)
            })
            .filter(|(argv, _)| argv.get(1).map(String::as_str) == Some(START))
            .collect()
    }

    #[test]
    fn a_shared_secret_is_named_by_services_and_run_but_never_said() {
        let w = world();
        let (code, _) = no(app(&w, "one", json!({"shared": "db_password"})));
        assert_eq!(code, "server.no_such_secret", "named before it is set");
        let (code, _) = no(act(
            &w.server,
            "server.secret.set",
            data!({"name": "Bad Name", "value": "x"}),
        ));
        assert_eq!(code, "server.bad_secret_name");

        let set = ok(act(
            &w.server,
            "server.secret.set",
            data!({"name": "db_password", "value": "first-pw"}),
        ));
        assert_eq!(set["used_by"], json!([]));
        ok(app(&w, "one", json!({"shared": "db_password"})));
        ok(app(&w, "two", json!({"shared": "db_password"})));
        let settings = ok(act(&w.server, "server.settings", Data::new()));
        assert_eq!(
            settings["secrets"],
            json!([{"name": "db_password", "used_by": ["one", "two"]}])
        );
        assert!(!json!(settings).to_string().contains("first-pw"));
        let listed = ok(act(&w.server, "service.list", Data::new()));
        assert_eq!(
            listed["services"][0]["config"]["token"],
            json!({"shared": "db_password"})
        );

        ok(act(&w.server, "service.start", data!({"service": "one"})));
        let (argv, conceal) = starts(&w).pop().unwrap();
        assert!(argv.contains(&"--token=first-pw".to_owned()));
        assert_eq!(conceal, json!(["first-pw"]));

        // Changed, it is what the next start runs with.
        ok(act(
            &w.server,
            "server.secret.set",
            data!({"name": "db_password", "value": "second-pw"}),
        ));
        ok(act(&w.server, "service.restart", data!({"service": "one"})));
        let (argv, conceal) = starts(&w).pop().unwrap();
        assert!(argv.contains(&"--token=second-pw".to_owned()));
        assert_eq!(conceal, json!(["second-pw"]));

        let (code, _) = no(act(
            &w.server,
            "server.secret.remove",
            data!({"name": "db_password"}),
        ));
        assert_eq!(code, "server.secret_in_use");
        for name in ["one", "two"] {
            ok(act(&w.server, "service.remove", data!({"service": name})));
        }
        ok(act(
            &w.server,
            "server.secret.remove",
            data!({"name": "db_password"}),
        ));
        let own = std::fs::read_to_string(w.room.join("own.json")).unwrap();
        assert!(!own.contains("db_password"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(w.room.join("own.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "own.json holds secrets");
        }
    }

    /// The guest agent makes files anyone may change; a runner among them
    /// runs as root. So each is left root's to change, and root's alone to
    /// read when a secret is in it.
    #[test]
    fn written_files_are_roots_and_a_secret_one_root_alone_reads() {
        let w = world();
        ok(act(
            &w.server,
            "service.create",
            data!({
                "name": "app",
                "config": {"token": "t0ken-value"},
                "definition": {
                    "id": "app", "version": "1", "name": "App",
                    "runtime": {"type": "process", "command": ["/srv/app"]},
                    "configuration": {"token": {"kind": "secret"}},
                    "files": [
                        {"path": "/etc/beatbox/{_id}/run.sh", "content": "echo hello"},
                        {"path": "/etc/beatbox/{_id}/token", "content": "{token}"}
                    ]
                }
            }),
        ));
        ok(act(&w.server, "service.start", data!({"service": "app"})));
        let id = format!("{}-app", w.server.id());
        let chmods: Vec<Vec<String>> = w
            .guest
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(capability, arguments)| {
                capability == "process.execute" && arguments["command"] == "chmod"
            })
            .map(|(_, arguments)| words(&arguments["arguments"]))
            .collect();
        assert_eq!(
            chmods,
            [
                vec![
                    "644".to_owned(),
                    "--".to_owned(),
                    format!("/etc/beatbox/{id}/run.sh")
                ],
                vec![
                    "600".to_owned(),
                    "--".to_owned(),
                    format!("/etc/beatbox/{id}/token")
                ],
            ]
        );
    }

    #[test]
    fn a_domain_is_checked_and_a_certificate_needs_one() {
        let w = world();
        for bad in ["Not A Domain!", "-lab.test", "lab..test"] {
            let (code, _) = no(act(&w.server, "server.settings", data!({"domain": bad})));
            assert_eq!(code, "server.bad_domain", "{bad}");
        }
        let (code, _) = no(act(
            &w.server,
            "server.settings",
            data!({"certificate": true}),
        ));
        assert_eq!(code, "server.certificate_needs_domain");
        let said = ok(act(
            &w.server,
            "server.settings",
            data!({"domain": "Srv.Lab.", "certificate": true}),
        ));
        assert_eq!(said["domain"], "srv.lab");
        assert_eq!(said["certificate"], true);
        // No domain, no shared certificate.
        let said = ok(act(&w.server, "server.settings", data!({"domain": ""})));
        assert_eq!(said["certificate"], false);
    }

    #[test]
    fn https_services_share_one_certificate_for_the_domain() {
        let w = world();
        let website = |name: &str, port: u16, https: u16| {
            ok(act(
                &w.server,
                "service.create",
                data!({"definition": "http-static", "name": name, "config": {"port": port, "https_port": https}}),
            ));
            ok(act(&w.server, "service.start", data!({"service": name})));
        };
        let certificates = |w: &World| -> Vec<Vec<String>> {
            w.guest
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|(capability, _)| capability == "process.execute")
                .map(|(_, arguments)| words(&arguments["arguments"]))
                .filter(|argv| argv.get(1).map(String::as_str) == Some(CERTIFICATE))
                .map(|argv| argv[3..].to_vec())
                .collect()
        };
        // Each its own, as before.
        website("plain", 8080, 8443);
        assert!(certificates(&w).is_empty());
        let own = format!("/etc/beatbox/{}-plain/tls", w.server.id());
        assert_eq!(starts(&w).pop().unwrap().0.last(), Some(&own));

        ok(act(
            &w.server,
            "server.settings",
            data!({"domain": "srv.lab", "certificate": true}),
        ));
        let folder = format!("/etc/beatbox/shared/{}/srv.lab", w.server.id());
        website("portal", 8081, 8444);
        website("intranet", 8082, 0);
        assert_eq!(
            certificates(&w),
            [vec![folder.clone(), "srv.lab".to_owned()]],
            "made for the one that answers HTTPS, the other not asked"
        );
        let all = starts(&w);
        assert_eq!(all[1].0.last(), Some(&folder));

        // No openssl: said, before anything runs.
        w.guest.lock().unwrap().missing_program = Some("openssl");
        let made = act(
            &w.server,
            "service.create",
            data!({"definition": "http-static", "name": "shop", "config": {"port": 8083, "https_port": 8445}}),
        );
        assert!(made.ok);
        let (code, values) = no(act(&w.server, "service.start", data!({"service": "shop"})));
        assert_eq!(code, "server.program_missing");
        assert_eq!(values["program"], "openssl");
    }
}
