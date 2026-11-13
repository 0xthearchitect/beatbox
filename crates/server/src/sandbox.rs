//! The Server sandbox and its ServiceManager.
//!
//! A Server holds service instances, each a [`Definition`] plus a
//! configuration, run through the runtime the definition names and never
//! through anything that knows what the service is.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use beatbox_engine::{
    Action, ActionSpec, Data, Emitter, Field, Isolation, Kind, Lifecycle, Observation, Peers,
    ProviderError, ProviderSpec, Reason, Requirement, SandboxConfig, SandboxIdentity,
    SandboxProvider, data, reason,
};
use std::net::{IpAddr, Ipv4Addr};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::definition::{Definition, Health, Runtime, SECRET_SHOWN, is_key, shared_secret};
use crate::node::{Node, Running, Shared};
use crate::placeholders::fill;
use crate::{builtins, internal, process};

/// Everything this type can ever do. Each past the lifecycle has an
/// [`ActionSpec`] in [`actions`].
pub const CAPABILITIES: &[&str] = &[
    "lifecycle.reset",
    "service.catalog",
    "service.create",
    "service.configure",
    "service.remove",
    "service.start",
    "service.stop",
    "service.restart",
    "service.list",
    "service.logs",
    "service.health",
    "service.describe",
    "service.call",
    "service.define",
    "service.depend",
    "service.supervise",
    "server.settings",
    "server.check",
    "server.secret.set",
    "server.secret.remove",
];

/// What the Host it uses must be able to do.
pub const HOST_NEEDS: &[&str] = &[
    "process.execute",
    "filesystem.read",
    "filesystem.write",
    "network.interfaces",
];
/// What the Network it uses must be able to do.
pub const NETWORK_NEEDS: &[&str] = &[
    "network.attach",
    "network.detach",
    "dns.add",
    "dns.remove",
    "dns.resolve",
];

/// What each capability past the lifecycle takes and gives back.
pub fn actions() -> Vec<ActionSpec> {
    use Kind::{Any, Bool, List, Number, Object, Text};
    let need = Field::required;
    let may = Field::optional;
    let one = |capability: &str, outputs: Vec<Field>| {
        ActionSpec::new(capability, vec![need("service", Text)], outputs)
    };
    let moved = || {
        vec![
            need("service", Text),
            need("state", Text),
            need("health", Text),
            may("pid", Number),
        ]
    };
    vec![
        ActionSpec::new(
            "service.catalog",
            vec![],
            vec![need("definitions", List), need("refused", List)],
        ),
        ActionSpec::new(
            "service.create",
            vec![
                need("definition", Any),
                need("name", Text),
                may("config", Object),
                may("after", List),
            ],
            vec![
                need("service", Text),
                need("definition", Text),
                need("state", Text),
            ],
        ),
        ActionSpec::new(
            "service.configure",
            vec![
                need("service", Text),
                need("config", Object),
                may("update_definition", Bool),
            ],
            vec![need("service", Text), need("config", Object)],
        ),
        ActionSpec::new(
            "service.remove",
            vec![need("service", Text), may("delete_data", Bool)],
            vec![need("service", Text), may("deleted", List)],
        ),
        one("service.start", moved()),
        one("service.stop", moved()),
        one("service.restart", moved()),
        one("service.health", moved()),
        one(
            "service.describe",
            vec![
                need("service", Text),
                need("definition", Object),
                need("config", Object),
                need("capabilities", List),
                need("data", List),
            ],
        ),
        // What comes back is the service capability's own: its outputs are
        // in `service.describe`, not here.
        ActionSpec::new(
            "service.call",
            vec![
                need("service", Text),
                need("capability", Text),
                may("arguments", Object),
            ],
            vec![need("service", Text), need("capability", Text)],
        ),
        ActionSpec::new(
            "service.define",
            vec![need("definition", Object), may("replace", Bool)],
            vec![need("id", Text), need("file", Text)],
        ),
        ActionSpec::new(
            "server.settings",
            vec![
                may("start_period_s", Number),
                may("restore", Bool),
                may("on_exit", Text),
                may("domain", Text),
                may("certificate", Bool),
            ],
            vec![
                need("start_period_s", Number),
                need("restore", Bool),
                need("on_exit", Text),
                need("domain", Text),
                need("certificate", Bool),
                need("secrets", List),
                may("library", Text),
            ],
        ),
        ActionSpec::new(
            "server.secret.set",
            vec![need("name", Text), need("value", Text)],
            vec![need("name", Text), need("used_by", List)],
        ),
        ActionSpec::new(
            "server.secret.remove",
            vec![need("name", Text)],
            vec![need("name", Text)],
        ),
        ActionSpec::new(
            "service.depend",
            vec![need("service", Text), may("after", List)],
            vec![need("service", Text), need("after", List)],
        ),
        ActionSpec::new("service.supervise", vec![], vec![need("restarted", List)]),
        ActionSpec::new(
            "server.check",
            vec![may("programs", List)],
            vec![may("host", Any), may("network", Any)],
        ),
        ActionSpec::new(
            "service.list",
            vec![may("check_health", Bool)],
            vec![need("services", List)],
        ),
        ActionSpec::new(
            "service.logs",
            vec![need("service", Text), may("lines", Number)],
            vec![need("service", Text), need("lines", List)],
        ),
    ]
}

/// The Server's own settings, changed while it runs (`server.settings`) and
/// kept beside its services: what a person sets on its Definições page.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Own {
    /// How long a service has to become healthy; the configuration's
    /// `start_period_s`, or [`START_PERIOD`], when not set.
    #[serde(default)]
    start_period_s: Option<f64>,
    /// Whether starting the Server starts again what was meant to run.
    #[serde(default = "yes")]
    restore: bool,
    /// Whether the Server itself was last started rather than stopped. The
    /// runtime looks at a stopped Server too, and that look must not bring
    /// its internal services back. Kept here because a Server brought back
    /// after a restart is never told which it was. None, from before this
    /// was kept: on, as it was then.
    #[serde(default)]
    on: Option<bool>,
    /// What to do with a service that stops on its own.
    #[serde(default)]
    on_exit: OnExit,
    /// The domain its services are named under on the network, as
    /// `<service>.<domain>`; empty for none.
    #[serde(default)]
    domain: String,
    /// Whether its HTTPS services share one certificate for the domain and
    /// every name under it, instead of each making its own.
    #[serde(default)]
    certificate: bool,
    /// Secrets its services name instead of holding (`{"shared": name}`):
    /// set once, never said back.
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

/// What the Server does with a service meant to run that is not running:
/// one that stopped on its own, or that its machine took down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OnExit {
    /// Leave it stopped, and say so: what it did before this was kept.
    #[default]
    Leave,
    /// Start it again, up to [`RETRIES`] times in a row.
    Retry,
    /// Start it again every time.
    Always,
}

impl OnExit {
    fn as_str(self) -> &'static str {
        match self {
            Self::Leave => "leave",
            Self::Retry => "retry",
            Self::Always => "always",
        }
    }
}

/// How many times in a row `retry` starts a service again.
const RETRIES: u32 = 3;
/// How long a restarted service has to keep running before its count of
/// restarts starts again, in milliseconds.
const STEADY_MS: u64 = 5 * 60 * 1000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn yes() -> bool {
    true
}

impl Default for Own {
    fn default() -> Self {
        Self {
            start_period_s: None,
            restore: true,
            on: None,
            on_exit: OnExit::Leave,
            domain: String::new(),
            certificate: false,
            secrets: BTreeMap::new(),
        }
    }
}

/// The longest start period a person may ask for, in seconds.
const MOST_PERIOD: f64 = 600.0;

/// Where a Server lives when nobody said.
fn default_room(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("beatbox-{id}"))
}

/// Where one service instance is in its own life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Made, never started.
    Created,
    /// Running, as far as was last seen.
    Running,
    /// Stopped on purpose.
    Stopped,
    /// Did not start, or died on its own.
    Failed,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

/// How a service was last found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wellness {
    /// Its check passed.
    Healthy,
    /// Running, and its check did not pass.
    Unhealthy,
    /// Not running, never checked, or with no check to run.
    #[default]
    Unknown,
}

impl Wellness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
        }
    }
}

/// How long a service has to become healthy after it starts, when the
/// Server's settings do not say (`start_period_s`).
pub const START_PERIOD: Duration = Duration::from_secs(10);
/// How long between two checks while it does.
const RECHECK: Duration = Duration::from_millis(500);

/// One service, as the Server keeps it in `services.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Instance {
    /// What it is called on this Server.
    name: String,
    /// `_id`: the Server's id and the name, stable for its whole life.
    id: String,
    /// Copied whole when it was made: a definition changed later does not
    /// change a service already made from it.
    definition: Definition,
    /// `builtin`, `library` or `inline`.
    source: String,
    /// Complete, defaults filled in.
    config: Data,
    /// Whether it is meant to be running, kept across a Server stop.
    wanted: bool,
    state: State,
    pid: Option<i64>,
    #[serde(default)]
    health: Wellness,
    #[serde(default)]
    health_checked_at: Option<u64>,
    /// What an internal service keeps across a restart: a DNS's records.
    #[serde(default)]
    memory: Value,
    /// Why its last start did not happen, until it starts or is stopped: a
    /// start the Server tried on its own (restoring) is said nowhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    problem: Option<Reason>,
    /// The services this one needs running first, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    after: Vec<String>,
    /// Starts the Server made on its own since it last ran steadily.
    #[serde(default)]
    restarts: u32,
    /// When the last of those was, in milliseconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restarted_at: Option<u64>,
    /// The run of its machine it was last seen running in
    /// ([`ServerSandbox::machine_run`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen_in: Option<String>,
}

impl Instance {
    fn values(&self) -> BTreeMap<String, String> {
        self.definition.values(&self.name, &self.id, &self.config)
    }

    /// Its secret values, to be run but never said: its own, and the
    /// shared ones it names.
    fn secrets(&self, shared: &BTreeMap<String, String>) -> Vec<String> {
        let mut secrets = self.definition.secrets(&self.config);
        secrets.extend(
            self.shared_secrets()
                .filter_map(|name| shared.get(name))
                .filter(|value| !value.is_empty())
                .cloned(),
        );
        secrets
    }

    /// The shared secrets it names.
    fn shared_secrets(&self) -> impl Iterator<Item = &str> {
        self.config.values().filter_map(shared_secret)
    }

    /// `text` with every secret value of it as [`SECRET_SHOWN`]: what a
    /// program printed, before it goes into a refusal, an event or a page.
    fn hide(&self, shared: &BTreeMap<String, String>, text: String) -> String {
        self.secrets(shared)
            .iter()
            .fold(text, |text, secret| text.replace(secret, SECRET_SHOWN))
    }

    /// What is its alone where it runs: its own folders and its log. Empty
    /// for an internal one, whose data is `memory`, gone with it.
    fn data(&self) -> Vec<String> {
        if self.definition.is_internal() {
            return Vec::new();
        }
        let values = self.values();
        let mut paths: Vec<String> = self
            .definition
            .own_folders()
            .iter()
            .map(|folder| fill(folder, &values))
            .collect();
        paths.push(format!("{}/{}.log", process::LOG_DIR, self.id));
        paths
    }

    fn summary(&self, address: Option<&str>) -> Value {
        json!({
            "service": self.name,
            "definition": self.definition.id,
            "definition_version": self.definition.version,
            "source": self.source,
            "state": self.state.as_str(),
            "health": self.health.as_str(),
            "health_checked_at": self.health_checked_at,
            "pid": self.pid,
            "config": self.definition.masked(&self.config),
            "endpoints": self.endpoints(address),
            "problem": self.problem,
            "wanted": self.wanted,
            "after": self.after,
            "restarts": self.restarts,
        })
    }

    /// Where it answers, while it runs: `[{service, protocol,
    /// transport, address, port}]`, `address` being where it runs.
    fn endpoints(&self, address: Option<&str>) -> Vec<Value> {
        let (State::Running, Some(address)) = (self.state, address) else {
            return Vec::new();
        };
        let values = self.values();
        self.definition
            .interfaces
            .iter()
            .filter_map(|interface| {
                let port = interface.port.resolve(&values)?;
                Some(json!({
                    "service": self.name,
                    "protocol": interface.protocol,
                    "transport": interface.transport.as_str(),
                    "address": address,
                    "port": port,
                }))
            })
            .collect()
    }
}

/// A Server: a host for services it knows nothing about.
///
/// Configured through `settings`:
///
/// | | |
/// |---|---|
/// | `workspace` | where it keeps `services.json`. A folder in the system's temporary one when not said. |
/// | `definitions` | a folder of definition files: the user's library, and how a declarative plugin is installed. |
/// | `start_period_s` | how long a service has to become healthy after it starts. 10 unless said. |
///
/// And through `uses`: `host`, for the services that run in one, and
/// `network`, for the ones it runs itself. Both optional.
pub struct ServerSandbox {
    identity: SandboxIdentity,
    config: SandboxConfig,
    emitter: Emitter,
    peers: Peers,
    room: PathBuf,
    services: Vec<Instance>,
    /// Addresses last requested explicitly from the guest, not from Host's
    /// possibly unrefreshed observation. Observe itself stays free of I/O.
    host_addresses: Option<Vec<String>>,
    /// The internal services running, shared with the node's thread.
    internals: Shared,
    /// Its node on the network, while an internal service runs.
    node: Option<Node>,
    /// Per internal service, the names it put in the network's own table.
    published: BTreeMap<String, BTreeMap<String, Ipv4Addr>>,
    /// Its own settings (`own.json`).
    own: Own,
}

impl ServerSandbox {
    /// `server`, version 1.
    ///
    /// Isolation `none`, honestly: a `process` service is as isolated as the
    /// machine it runs in, and an `internal` one runs in BeatBox's own
    /// process. The Server promises nothing more than that.
    pub fn spec() -> ProviderSpec {
        ProviderSpec::new(
            "server",
            "1",
            CAPABILITIES,
            Isolation::None,
            |identity, config, emitter| Box::new(Self::new(identity, config, emitter)),
        )
        .with_actions(&actions())
        .with_requires(&[
            Requirement::new("host", HOST_NEEDS).optional(),
            Requirement::new("network", NETWORK_NEEDS).optional(),
        ])
    }

    fn new(identity: SandboxIdentity, config: SandboxConfig, emitter: Emitter) -> Self {
        let room = config
            .setting("workspace")
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty())
            .map_or_else(|| default_room(&identity.id), PathBuf::from);
        // Brought back after a restart, the services are where they were
        // left; nothing in a guest is asked until somebody asks.
        let services = std::fs::read(room.join(SERVICES))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let own = std::fs::read(room.join(OWN))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            identity,
            config,
            emitter,
            peers: Peers::default(),
            room,
            services,
            host_addresses: None,
            internals: Shared::default(),
            node: None,
            published: BTreeMap::new(),
            own,
        }
    }

    /// The folder of the user's own definitions, when it has one.
    fn library(&self) -> Option<PathBuf> {
        self.config
            .setting("definitions")
            .and_then(Value::as_str)
            .filter(|folder| !folder.is_empty())
            .map(PathBuf::from)
    }

    /// Read or change the Server's own settings.
    fn settings(&mut self, action: &Action) -> Result<Data, Reason> {
        if let Some(seconds) = action.argument("start_period_s").and_then(Value::as_f64) {
            if !(1.0..=MOST_PERIOD).contains(&seconds) {
                return Err(reason!("server.bad_period", most = MOST_PERIOD));
            }
            self.own.start_period_s = Some(seconds);
        }
        if let Some(restore) = action.argument("restore").and_then(Value::as_bool) {
            self.own.restore = restore;
        }
        if let Some(asked) = action.argument("on_exit").and_then(Value::as_str) {
            self.own.on_exit = match asked {
                "leave" => OnExit::Leave,
                "retry" => OnExit::Retry,
                "always" => OnExit::Always,
                _ => return Err(reason!("server.bad_on_exit", value = asked)),
            };
        }
        if let Some(asked) = action.argument("domain").and_then(Value::as_str) {
            let domain = asked.trim().trim_end_matches('.').to_ascii_lowercase();
            if !domain.is_empty() && !is_domain(&domain) {
                return Err(reason!("server.bad_domain", domain = asked));
            }
            // A certificate for no domain would be each service's own again.
            if domain.is_empty() {
                self.own.certificate = false;
            }
            self.own.domain = domain;
        }
        if let Some(shared) = action.argument("certificate").and_then(Value::as_bool) {
            if shared && self.own.domain.is_empty() {
                return Err(reason!("server.certificate_needs_domain"));
            }
            self.own.certificate = shared;
        }
        if !action.arguments.is_empty() {
            self.save_own()?;
            self.publish_domain();
        }
        let secrets: Vec<Value> = self
            .own
            .secrets
            .keys()
            .map(|name| json!({"name": name, "used_by": self.users(name)}))
            .collect();
        Ok(data!({
            "start_period_s": self.start_period().as_secs_f64(),
            "restore": self.own.restore,
            "on_exit": self.own.on_exit.as_str(),
            "domain": self.own.domain,
            "certificate": self.own.certificate,
            "secrets": secrets,
            "library": self.library().map(|folder| folder.to_string_lossy().into_owned()),
            "library_shown": self.library().map(|folder| from_home(&folder, std::env::home_dir())),
        }))
    }

    /// Kept like the services: written whole, then put in place, readable by
    /// its owner only -- it holds the shared secrets.
    fn save_own(&self) -> Result<(), Reason> {
        let fail = |error: std::io::Error| reason!("server.could_not_save", detail = error);
        std::fs::create_dir_all(&self.room).map_err(fail)?;
        let text = serde_json::to_string_pretty(&self.own).expect("settings serialise");
        let fresh = self.room.join(format!("{OWN}.new"));
        write_private(&fresh, text.as_bytes()).map_err(fail)?;
        std::fs::rename(&fresh, self.room.join(OWN)).map_err(fail)
    }

    /// The services that name shared secret `name`.
    fn users(&self, name: &str) -> Vec<String> {
        self.services
            .iter()
            .filter(|each| each.shared_secrets().any(|used| used == name))
            .map(|each| each.name.clone())
            .collect()
    }

    /// Set a shared secret, new or changed. A service that names it gets
    /// the new value at its next start.
    fn set_secret(&mut self, action: &Action) -> Result<Data, Reason> {
        let name = text(action, "name");
        if !is_key(&name) || name.len() > 40 {
            return Err(reason!("server.bad_secret_name", name = name));
        }
        let value = action
            .argument("value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if value.is_empty() {
            return Err(reason!("server.secret_empty", name = name));
        }
        self.own.secrets.insert(name.clone(), value.to_owned());
        self.save_own()?;
        Ok(data!({"name": name, "used_by": self.users(&name)}))
    }

    /// Forget a shared secret no service names.
    fn remove_secret(&mut self, action: &Action) -> Result<Data, Reason> {
        let name = text(action, "name");
        if !self.own.secrets.contains_key(&name) {
            return Err(reason!("server.no_such_secret", name = name));
        }
        if let Some(by) = self.users(&name).first() {
            return Err(reason!("server.secret_in_use", name = name, by = by));
        }
        self.own.secrets.remove(&name);
        self.save_own()?;
        Ok(data!({"name": name}))
    }

    /// Every shared secret `config` names, known to this Server.
    fn check_shared(&self, service: &str, config: &Data) -> Result<(), Reason> {
        match config
            .values()
            .filter_map(shared_secret)
            .find(|name| !self.own.secrets.contains_key(*name))
        {
            Some(missing) => Err(reason!(
                "server.no_such_secret",
                name = missing,
                service = service
            )),
            None => Ok(()),
        }
    }

    /// What every placeholder of `at` becomes when it runs: its own values,
    /// the shared secrets it names, and the shared certificate's folder
    /// when there is one.
    fn run_values(&self, at: usize) -> BTreeMap<String, String> {
        let instance = &self.services[at];
        let mut values = instance.values();
        for (key, value) in &instance.config {
            if let Some(name) = shared_secret(value) {
                let known = self.own.secrets.get(name).cloned().unwrap_or_default();
                values.insert(key.clone(), known);
            }
        }
        if let Some(folder) = self.shared_tls() {
            values.insert("_tls".into(), folder);
        }
        values
    }

    /// Where the shared certificate is in the machine, when there is one:
    /// a folder per domain, so a new domain is a new certificate.
    fn shared_tls(&self) -> Option<String> {
        (self.own.certificate && !self.own.domain.is_empty()).then(|| {
            format!(
                "/etc/beatbox/shared/{}/{}",
                self.identity.id, self.own.domain
            )
        })
    }

    /// What it needs from the sandboxes it uses, as they are now: each
    /// one's state, and for a running machine with its agent, which of
    /// `programs` -- every definition's `requires` when not said; names on
    /// the PATH or absolute paths -- it has. One run in the guest, and only
    /// when asked: a page that shows it, never an observation.
    fn check(&mut self, action: &Action) -> Result<Data, Reason> {
        let lifecycle = |role: &str| {
            self.peers.state(role).map(|state| {
                serde_json::to_value(state.lifecycle)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            })
        };
        let network = self.peers.id("network").map(
            |id| json!({"id": id, "state": lifecycle("network").unwrap_or_else(|| "gone".into())}),
        );
        let Some(id) = self.peers.id("host").map(str::to_owned) else {
            return Ok(data!({"host": Value::Null, "network": network}));
        };
        let state = lifecycle("host").unwrap_or_else(|| "gone".into());
        let mut asked: Vec<String> = match action.argument("programs") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => self
                .catalog()
                .0
                .into_iter()
                .flat_map(|(definition, _)| definition.requires)
                .collect(),
        };
        asked.retain(|program| !program.is_empty());
        asked.sort();
        asked.dedup();
        // The agent answers when this run does: the Host's own fact is only
        // said when it is false, and only as of the last observation.
        let mut agent = false;
        let mut programs = serde_json::Map::new();
        if state == "running" {
            let mut argv = vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                process::HAS.to_owned(),
                "sh".to_owned(),
            ];
            argv.extend(asked.iter().cloned());
            if let Ok(said) = process::execute(&self.peers, &argv, &[]) {
                agent = true;
                let found: Vec<&str> = said
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .lines()
                    .collect();
                for program in &asked {
                    programs.insert(
                        program.clone(),
                        Value::Bool(found.contains(&program.as_str())),
                    );
                }
            }
        }
        Ok(data!({
            "host": {"id": id, "state": state, "agent": agent, "programs": programs},
            "network": network,
        }))
    }

    /// Write a definition into the library, checked first:
    /// what a person made in a form becomes a file like any plugin's.
    fn define(&mut self, action: &Action) -> Result<Data, Reason> {
        let Some(folder) = self.library() else {
            return Err(reason!("server.no_library"));
        };
        let definition = Definition::from_value(&action.arguments["definition"])?;
        if builtins().iter().any(|each| each.id == definition.id) {
            return Err(reason!("server.define_builtin", definition = definition.id));
        }
        let file = folder.join(format!("{}.json", definition.id));
        let replace = action.argument("replace").and_then(Value::as_bool) == Some(true);
        if file.exists() && !replace {
            return Err(reason!("server.define_exists", definition = definition.id));
        }
        let fail = |error: std::io::Error| reason!("server.could_not_save", detail = error);
        std::fs::create_dir_all(&folder).map_err(fail)?;
        let text = serde_json::to_string_pretty(&definition).expect("a definition serialises");
        std::fs::write(&file, text + "\n").map_err(fail)?;
        let file = file.to_string_lossy().into_owned();
        self.emit(
            "service.defined",
            data!({"id": definition.id, "file": file}),
        );
        Ok(data!({"id": definition.id, "file": file}))
    }

    fn emit(&self, name: &str, data: Data) {
        if let Err(error) = self.emitter.emit(name, data) {
            log::warn!("{} could not say {name}: {error}", self.identity.id);
        }
    }

    /// Every definition it can make a service from: built in, then the
    /// library's; and the library's files that were refused. A file with a
    /// built-in's id is one of them: it would be shown and picked as the
    /// one BeatBox brings while running whatever it says.
    fn catalog(&self) -> (Vec<(Definition, &'static str)>, Vec<Value>) {
        let mut found: BTreeMap<String, (Definition, &'static str)> = builtins()
            .into_iter()
            .map(|d| (d.id.clone(), (d, "builtin")))
            .collect();
        let mut refused = Vec::new();
        let folder = self
            .config
            .setting("definitions")
            .and_then(Value::as_str)
            .filter(|f| !f.is_empty());
        let mut files: Vec<PathBuf> = folder
            .and_then(|folder| std::fs::read_dir(folder).ok())
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            match Definition::parse(&text) {
                Ok(definition)
                    if found
                        .get(&definition.id)
                        .is_some_and(|(_, s)| *s == "builtin") =>
                {
                    let why = reason!("server.define_builtin", definition = definition.id);
                    refused.push(json!({"file": path.display().to_string(), "reason": why}));
                }
                Ok(definition) => {
                    found.insert(definition.id.clone(), (definition, "library"));
                }
                Err(why) => {
                    refused.push(json!({"file": path.display().to_string(), "reason": why}))
                }
            }
        }
        (found.into_values().collect(), refused)
    }

    fn save(&self) -> Result<(), Reason> {
        let fail = |error: std::io::Error| reason!("server.could_not_save", detail = error);
        std::fs::create_dir_all(&self.room).map_err(fail)?;
        let text = serde_json::to_string_pretty(&self.services).expect("services serialise");
        let fresh = self.room.join(format!("{SERVICES}.new"));
        write_private(&fresh, text.as_bytes()).map_err(fail)?;
        std::fs::rename(&fresh, self.room.join(SERVICES)).map_err(fail)
    }

    fn find(&self, name: &str) -> Result<usize, Reason> {
        self.services
            .iter()
            .position(|each| each.name == name)
            .ok_or_else(|| reason!("server.no_such_service", service = name))
    }

    // --- the actions ---------------------------------------------------------

    fn create_service(&mut self, action: &Action) -> Result<Data, Reason> {
        let name = text(action, "name");
        if !is_key(&name.replace('-', "_")) || name.len() > 40 {
            return Err(reason!("server.bad_name", name = name));
        }
        if self.services.iter().any(|each| each.name == name) {
            return Err(reason!("server.name_taken", name = name));
        }
        let (definition, source) = match action.argument("definition") {
            Some(Value::Object(_)) => (
                Definition::from_value(&action.arguments["definition"])?,
                "inline".to_owned(),
            ),
            asked => {
                let id = asked.map(crate::definition::as_text).unwrap_or_default();
                let (all, _) = self.catalog();
                let (definition, source) = all
                    .into_iter()
                    .find(|(d, _)| d.id == id)
                    .ok_or_else(|| reason!("server.no_such_definition", definition = id))?;
                (definition, source.to_owned())
            }
        };
        let needs = if definition.is_internal() {
            "network"
        } else {
            "host"
        };
        if self.peers.id(needs).is_none() {
            return Err(match needs {
                "network" => reason!("server.needs_network", service = name),
                _ => reason!("server.needs_host", service = name),
            });
        }
        let given = action
            .argument("config")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let config = definition.configure(&name, &given)?;
        self.check_shared(&name, &config)?;
        let after = self.check_after(None, &name, &words(action, "after"))?;
        let instance = Instance {
            id: format!("{}-{name}", self.identity.id),
            name: name.clone(),
            definition,
            source,
            config,
            wanted: false,
            state: State::Created,
            pid: None,
            health: Wellness::Unknown,
            health_checked_at: None,
            memory: Value::Null,
            problem: None,
            after,
            restarts: 0,
            restarted_at: None,
            seen_in: None,
        };
        let said =
            data!({"service": name, "definition": instance.definition.id, "state": "created"});
        self.services.push(instance);
        self.save()?;
        self.emit("service.created", said.clone());
        Ok(said)
    }

    fn configure_service(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let instance = &self.services[at];
        if instance.state == State::Running {
            return Err(reason!(
                "server.configure_while_running",
                service = instance.name
            ));
        }
        let given = action
            .argument("config")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut updated = instance.config.clone();
        updated.extend(given.clone());
        let replacement = if action
            .argument("update_definition")
            .and_then(Value::as_bool)
            == Some(true)
        {
            let id = instance.definition.id.clone();
            let (all, _) = self.catalog();
            Some(
                all.into_iter()
                    .find(|(definition, _)| definition.id == id)
                    .ok_or_else(|| reason!("server.no_such_definition", definition = id))?,
            )
        } else {
            None
        };
        let definition = replacement
            .as_ref()
            .map(|(definition, _)| definition)
            .unwrap_or(&instance.definition);
        if definition.is_internal() != instance.definition.is_internal() {
            return Err(reason!("server.prepare_failed", service = instance.name));
        }
        let config = definition.configure(&instance.name, &updated)?;
        self.check_shared(&instance.name, &config)?;
        let keys: Vec<String> = given.keys().cloned().collect();
        self.services[at].config = config;
        if let Some((definition, source)) = replacement {
            self.services[at].definition = definition;
            self.services[at].source = source.to_owned();
        }
        self.save()?;
        let instance = &self.services[at];
        self.emit(
            "service.configured",
            data!({"service": instance.name, "keys": keys}),
        );
        Ok(
            data!({"service": instance.name, "config": instance.definition.masked(&instance.config)}),
        )
    }

    /// Stop it and forget it. Its data where it ran stays, for a service
    /// made again with the same name, unless `delete_data`: then it goes
    /// first, and a machine that cannot be asked keeps the service, so
    /// nothing is forgotten that was not deleted.
    fn remove_service(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let name = self.services[at].name.clone();
        if let Some(by) = self.services.iter().find(|each| each.after.contains(&name)) {
            return Err(reason!("server.still_needed", service = name, by = by.name));
        }
        self.halt(at)?;
        let paths = self.services[at].data();
        let delete = action.argument("delete_data").and_then(Value::as_bool) == Some(true)
            && !paths.is_empty();
        if delete {
            let mut argv = vec!["rm".to_owned(), "-rf".to_owned(), "--".to_owned()];
            argv.extend(paths.iter().cloned());
            process::execute(&self.peers, &argv, &[]).map_err(|why| match why.code.as_str() {
                "server.host_unavailable" | "server.host_gone" => reason!(
                    "server.delete_data_needs_host",
                    service = self.services[at].name
                ),
                _ => why,
            })?;
        }
        let gone = self.services.remove(at);
        self.save()?;
        self.emit(
            "service.removed",
            data!({"service": gone.name, "definition": gone.definition.id}),
        );
        let mut said = data!({"service": gone.name});
        if delete {
            said.insert("deleted".into(), json!(paths));
        }
        Ok(said)
    }

    fn moved(&self, at: usize) -> Data {
        let instance = &self.services[at];
        data!({
            "service": instance.name,
            "state": instance.state.as_str(),
            "health": instance.health.as_str(),
            "pid": instance.pid,
        })
    }

    /// A usable address of the guest, never its loopback interface.
    /// Prefer IPv4; IPv6 link-local needs an interface scope we do not have.
    fn host_address(&self) -> Option<String> {
        let known = self.host_addresses.clone().or_else(|| {
            let state = self.peers.state("host")?;
            Some(
                state
                    .fact("addresses")?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            )
        })?;
        let addresses: Vec<IpAddr> = known
            .iter()
            .filter_map(|address| address.parse::<IpAddr>().ok())
            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast())
            .filter(|ip| match ip {
                IpAddr::V4(ip) => !ip.is_link_local() && !ip.is_broadcast(),
                IpAddr::V6(ip) => !ip.is_unicast_link_local(),
            })
            .collect();
        addresses
            .iter()
            .find(|ip| ip.is_ipv4())
            .or_else(|| addresses.first())
            .map(ToString::to_string)
    }

    /// Read current interfaces on an explicit service request. Host actions
    /// do not refresh its observed facts, so successfully running a process
    /// does not imply its cached `addresses` have ever been populated.
    fn discover_host_addresses(&mut self) {
        let addresses = self
            .peers
            .act("host", "network.interfaces", Data::new())
            .ok()
            .filter(|outcome| outcome.ok)
            .and_then(|outcome| outcome.data.get("interfaces").cloned())
            .and_then(|interfaces| interfaces.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .flat_map(|interface| {
                interface
                    .get("addresses")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter_map(|address| address.get("address").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        self.host_addresses = Some(addresses);
    }

    /// Where a service answers: its Host's address for a `process` one,
    /// the node's for an `internal` one.
    fn address_of(&self, internal: bool) -> Option<String> {
        if internal {
            self.node
                .as_ref()
                .filter(|node| node.alive())
                .map(|node| node.address.to_string())
        } else {
            self.host_address()
        }
    }

    /// Whether the network its internal services answer on is there and on.
    fn network_up(&self) -> bool {
        self.peers
            .state("network")
            .is_some_and(|state| state.lifecycle == Lifecycle::Running)
    }

    fn registered(&self, name: &str) -> bool {
        self.node.as_ref().is_some_and(Node::alive)
            && self
                .internals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(name)
    }

    /// The node, cabled to the network, if it is not already.
    fn ensure_node(&mut self) -> Result<(), Reason> {
        if self.node.as_ref().is_some_and(Node::alive) {
            return Ok(());
        }
        // Named after the Server's id, never its name: an attach to a name
        // that exists hands back that node's cable, and a Server called
        // like a Host would take the Host's.
        let outcome = self
            .peers
            .act(
                "network",
                "network.attach",
                data!({"node": self.identity.id}),
            )
            .expect("network.attach is a valid name");
        if !outcome.ok {
            return Err(outcome
                .reason
                .unwrap_or_else(|| reason!("engine.unsupported", capability = "network.attach")));
        }
        let node = Node::start(
            &outcome.data,
            Arc::clone(&self.internals),
            &self.identity.id,
        )
        .map_err(|error| reason!("server.node_failed", detail = error))?;
        self.node = Some(node);
        Ok(())
    }

    /// The network's own table brought in line with the names internal
    /// service `name` gives while it runs (none once it is stopped). The
    /// network's DNS is the one DHCP hands every machine, so this is what
    /// makes the names work without configuring anything on them.
    ///
    /// A name the network already answers -- a node, or a record someone
    /// added there -- is left alone, and only what was added here is ever
    /// taken back. Best effort: the service answers on its own address
    /// either way.
    // A network seen off or gone marks its internal services stopped, and
    // `revive` runs them -- and so publishes again -- when it is back.
    fn publish(&mut self, name: &str) {
        let wanted = self
            .internals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .map(|running| running.implementation.names())
            .unwrap_or_default();
        self.put_names(name, wanted);
    }

    /// The domain's names: `<service>.<domain>` for each service
    /// running, at the address it answers on, while the Server is on. Kept
    /// as the names of `""`, beside the internal services' own; a look with
    /// nothing changed asks the network nothing.
    fn publish_domain(&mut self) {
        let mut wanted = BTreeMap::new();
        if !self.own.domain.is_empty() && self.own.on != Some(false) {
            for each in &self.services {
                if each.state != State::Running {
                    continue;
                }
                if let Some(address) = self
                    .address_of(each.definition.is_internal())
                    .and_then(|address| address.parse::<Ipv4Addr>().ok())
                {
                    wanted.insert(format!("{}.{}", each.name, self.own.domain), address);
                }
            }
        }
        self.put_names("", wanted);
    }

    /// The names `key` puts in the network's table brought in line with
    /// `wanted`.
    fn put_names(&mut self, key: &str, wanted: BTreeMap<String, Ipv4Addr>) {
        let mut mine = self.published.remove(key).unwrap_or_default();
        if wanted.is_empty() && mine.is_empty() {
            return;
        }
        let ask = |capability: &str, arguments: Data| {
            self.peers
                .act("network", capability, arguments)
                .ok()
                .filter(|outcome| outcome.ok)
        };
        mine.retain(|each, address| {
            let keep = wanted.get(each) == Some(address);
            if !keep {
                ask("dns.remove", data!({"name": each}));
            }
            keep
        });
        for (each, address) in wanted {
            if mine.contains_key(&each) {
                continue;
            }
            let free = ask("dns.resolve", data!({"name": each}))
                .is_some_and(|outcome| outcome.data.get("address").is_none_or(Value::is_null));
            // `from`: the network shows the name as this Server's, and keeps
            // it out of what it writes down as its own.
            if free
                && ask(
                    "dns.add",
                    data!({"name": each, "address": address.to_string(), "from": self.identity.id}),
                )
                .is_some()
            {
                mine.insert(each, address);
            }
        }
        if !mine.is_empty() {
            self.published.insert(key.to_string(), mine);
        }
    }

    /// The node gone, and the cable given back, once no internal service
    /// needs it.
    fn drop_node_if_idle(&mut self) {
        let idle = self
            .internals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty();
        if idle && let Some(node) = self.node.take() {
            node.stop();
            let _ = self.peers.act(
                "network",
                "network.detach",
                data!({"node": self.identity.id}),
            );
        }
    }

    /// Run an internal service: on the node, its ports given to its
    /// implementation, which picks up from what it remembered.
    fn launch_internal(&mut self, at: usize) -> Result<(), Reason> {
        let instance = &self.services[at];
        let name = instance.name.clone();
        if self.peers.id("network").is_none() {
            return Err(reason!("server.needs_network", service = name));
        }
        if self
            .peers
            .state("network")
            .is_some_and(|state| state.lifecycle != Lifecycle::Running)
        {
            return Err(reason!("server.network_not_running", service = name));
        }
        let values = instance.values();
        self.check_ports(at, &values)?;
        let ports: Vec<u16> = instance
            .definition
            .interfaces
            .iter()
            .filter_map(|each| each.port.resolve(&values))
            .collect();
        let Runtime::Internal { implementation } = &instance.definition.runtime else {
            unreachable!("launch_internal is for internal services");
        };
        let implementation = internal::make(implementation, &instance.memory)
            .expect("a loaded definition names an implementation this build has");
        self.ensure_node()?;
        self.internals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                name.clone(),
                Running {
                    ports,
                    implementation,
                },
            );
        self.publish(&name);
        let instance = &mut self.services[at];
        instance.state = State::Running;
        instance.pid = None;
        let endpoints = self.services[at].endpoints(self.address_of(true).as_deref());
        self.emit(
            "service.started",
            data!({"service": name, "endpoints": endpoints}),
        );
        self.settle(at)
    }

    /// Internal services meant to run and not running -- the daemon that ran
    /// them went away -- run again. Best effort: a network not back yet is
    /// tried again at the next look.
    fn revive(&mut self) {
        if self.own.on == Some(false) {
            return;
        }
        for at in 0..self.services.len() {
            let each = &self.services[at];
            if each.wanted
                && each.definition.is_internal()
                && (each.state != State::Running || !self.registered(&each.name))
                && self.network_up()
                && let Err(why) = self.launch(at)
            {
                log::debug!(
                    "{} could not bring {} back yet: {why:?}",
                    self.identity.id,
                    self.services[at].name
                );
            }
        }
    }

    fn start_period(&self) -> Duration {
        self.own
            .start_period_s
            .or_else(|| {
                self.config
                    .setting("start_period_s")
                    .and_then(Value::as_f64)
            })
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map_or(START_PERIOD, Duration::from_secs_f64)
    }

    /// Say a change of health, if it is one.
    fn set_health(&mut self, at: usize, now: Wellness) {
        let instance = &mut self.services[at];
        let was = instance.health;
        if was == now {
            return;
        }
        instance.health = now;
        let name = instance.name.clone();
        self.emit(
            "service.health.changed",
            data!({"service": name, "from": was.as_str(), "to": now.as_str()}),
        );
    }

    /// Run its check once, as the definition says, where it runs. Unknown
    /// for a service with no check to run.
    fn probe(&self, at: usize) -> Result<Wellness, Reason> {
        let instance = &self.services[at];
        let values = self.run_values(at);
        let port = |port: &crate::definition::Port| {
            port.resolve(&values)
                .ok_or_else(|| reason!("server.bad_port", service = instance.name))
        };
        let passed = match &instance.definition.health {
            None => return Ok(Wellness::Unknown),
            Some(Health::Internal) => {
                return Ok(if self.registered(&instance.name) {
                    Wellness::Healthy
                } else {
                    Wellness::Unknown
                });
            }
            Some(Health::Tcp { port: at }) => process::tcp_check(&self.peers, port(at)?)?,
            Some(Health::Http { port: at, path }) => {
                process::http_check(&self.peers, port(at)?, &fill(path, &values))?
            }
            Some(Health::Command { command }) => {
                let argv: Vec<String> = command.iter().map(|w| fill(w, &values)).collect();
                process::command_check(&self.peers, &argv, &instance.secrets(&self.own.secrets))?
            }
        };
        Ok(if passed {
            Wellness::Healthy
        } else {
            Wellness::Unhealthy
        })
    }

    /// Check it now: unknown when it is not running, its check otherwise.
    fn check_health(&mut self, at: usize) -> Result<Wellness, Reason> {
        let now = if self.running(at)? {
            self.probe(at)?
        } else {
            Wellness::Unknown
        };
        self.services[at].health_checked_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        );
        self.set_health(at, now);
        Ok(now)
    }

    /// After a start: check until it is healthy, it dies, or the start
    /// period is over -- then it is whatever it was last found to be.
    /// Running and unhealthy is said, not punished: the Server does not
    /// kill a service for being slow.
    fn settle(&mut self, at: usize) -> Result<(), Reason> {
        if self.services[at].definition.health.is_none() {
            return Ok(());
        }
        let deadline = Instant::now() + self.start_period();
        loop {
            let now = self.check_health(at)?;
            if now != Wellness::Unhealthy || Instant::now() >= deadline {
                return Ok(());
            }
            std::thread::sleep(RECHECK.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    /// Start one, and remember it is meant to run.
    fn start_service(&mut self, at: usize) -> Result<Data, Reason> {
        self.start_needs(at, &mut Vec::new())?;
        let instance = &mut self.services[at];
        instance.wanted = true;
        instance.restarts = 0;
        instance.restarted_at = None;
        self.try_launch(at)?;
        self.save()?;
        Ok(self.moved(at))
    }

    /// What `at` needs, started first -- theirs before them -- and
    /// answering: a service only starts on the ones it needs.
    fn start_needs(&mut self, at: usize, path: &mut Vec<usize>) -> Result<(), Reason> {
        if path.contains(&at) {
            return Ok(());
        }
        path.push(at);
        let name = self.services[at].name.clone();
        for need in self.services[at].after.clone() {
            let Ok(dep) = self.find(&need) else { continue };
            self.start_needs(dep, path)?;
            if !self.running(dep)? {
                self.services[dep].wanted = true;
                if self.try_launch(dep).is_err() {
                    return Err(reason!("server.after_failed", service = name, after = need));
                }
            }
            if self.services[dep].health == Wellness::Unhealthy {
                return Err(reason!("server.after_silent", service = name, after = need));
            }
        }
        path.pop();
        Ok(())
    }

    /// `after` for the service `name`, checked: services it has, never
    /// itself, and nothing that needs `name` back. Blanks and repeats go.
    fn check_after(
        &self,
        at: Option<usize>,
        name: &str,
        after: &[String],
    ) -> Result<Vec<String>, Reason> {
        let mut clean: Vec<String> = Vec::new();
        for each in after {
            let each = each.trim();
            if each.is_empty() || clean.iter().any(|kept| kept == each) {
                continue;
            }
            if each == name {
                return Err(reason!("server.after_itself", service = name));
            }
            if !self.services.iter().any(|service| service.name == each) {
                return Err(reason!(
                    "server.after_unknown",
                    service = name,
                    after = each
                ));
            }
            clean.push(each.to_owned());
        }
        if at.is_some() {
            let mut seen = BTreeSet::new();
            let mut todo = clean.clone();
            while let Some(next) = todo.pop() {
                if next == name {
                    return Err(reason!("server.after_loop", service = name));
                }
                if seen.insert(next.clone())
                    && let Some(service) = self.services.iter().find(|service| service.name == next)
                {
                    todo.extend(service.after.iter().cloned());
                }
            }
        }
        Ok(clean)
    }

    /// Change what a service needs, at any time: it applies at its next start.
    fn depend(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let name = self.services[at].name.clone();
        let after = self.check_after(Some(at), &name, &words(action, "after"))?;
        self.services[at].after = after.clone();
        self.save()?;
        self.emit(
            "service.configured",
            data!({"service": name, "keys": ["after"]}),
        );
        Ok(data!({"service": name, "after": after}))
    }

    /// Every service, each after the ones it needs; otherwise as listed.
    fn order(&self) -> Vec<usize> {
        fn visit(this: &ServerSandbox, at: usize, done: &mut Vec<usize>, path: &mut Vec<usize>) {
            if done.contains(&at) || path.contains(&at) {
                return;
            }
            path.push(at);
            for need in &this.services[at].after {
                if let Some(dep) = this.services.iter().position(|each| &each.name == need) {
                    visit(this, dep, done, path);
                }
            }
            path.pop();
            done.push(at);
        }
        let mut done = Vec::new();
        for at in 0..self.services.len() {
            visit(self, at, &mut done, &mut Vec::new());
        }
        done
    }

    /// Whether every service `at` needs is running.
    fn needs_running(&self, at: usize) -> Option<String> {
        self.services[at]
            .after
            .iter()
            .find(|need| {
                self.services
                    .iter()
                    .any(|each| &each.name == *need && each.state != State::Running)
            })
            .cloned()
    }

    /// Bring back, as the Server's settings say, what is meant to run and
    /// does not: one that stopped on its own, or that its machine took
    /// down. The daemon asks every little while; set to `leave`, or with the
    /// Server off, it asks nothing of anybody.
    fn supervise(&mut self) -> Result<Data, Reason> {
        let mut restarted: Vec<Value> = Vec::new();
        if self.own.on_exit == OnExit::Leave || self.own.on == Some(false) {
            return Ok(data!({"restarted": restarted}));
        }
        // A machine that is off cannot be asked: its services wait for it.
        let host_up = self
            .peers
            .state("host")
            .is_some_and(|state| state.lifecycle == Lifecycle::Running);
        if !host_up {
            return Ok(data!({"restarted": restarted}));
        }
        for at in self.order() {
            let instance = &self.services[at];
            if !instance.wanted || instance.definition.is_internal() {
                continue;
            }
            match self.running(at) {
                Ok(true) => {
                    let instance = &mut self.services[at];
                    if instance
                        .restarted_at
                        .is_some_and(|when| now_ms().saturating_sub(when) > STEADY_MS)
                    {
                        instance.restarts = 0;
                        instance.restarted_at = None;
                    }
                    continue;
                }
                Ok(false) => {}
                Err(_) => continue,
            }
            let instance = &self.services[at];
            // A start that was refused -- a program missing, a folder that is
            // not there -- is the same refusal every time: it waits for the
            // person, instead of being asked again every round, for ever. A
            // machine whose agent is not up yet is not one of those.
            let refused = instance.problem.as_ref().is_some_and(|why| {
                !matches!(
                    why.code.as_str(),
                    "server.died" | "server.did_not_start" | "host.no_agent"
                )
            });
            if !matches!(instance.state, State::Failed | State::Stopped)
                || refused
                || (self.own.on_exit == OnExit::Retry && instance.restarts >= RETRIES)
                || self.needs_running(at).is_some()
            {
                continue;
            }
            let name = instance.name.clone();
            let attempt = instance.restarts + 1;
            let instance = &mut self.services[at];
            instance.restarts = attempt;
            instance.restarted_at = Some(now_ms());
            self.emit(
                "service.restarting",
                data!({"service": name, "attempt": attempt}),
            );
            match self.try_launch(at) {
                Ok(()) => restarted.push(Value::from(name)),
                // A machine still booting has no agent yet: not the
                // service's failure, so not one of its tries.
                Err(why) if why.code == "host.no_agent" => self.services[at].restarts -= 1,
                Err(_) => {}
            }
        }
        self.save()?;
        Ok(data!({"restarted": restarted}))
    }

    /// Launch it, and keep why it did not start, so the page can say so
    /// after a start nobody was watching. A machine or network that is off
    /// is not the service's failure: it stays as it was.
    fn try_launch(&mut self, at: usize) -> Result<(), Reason> {
        let done = self.launch(at);
        let instance = &mut self.services[at];
        match &done {
            Ok(()) => instance.problem = None,
            Err(why)
                if matches!(
                    why.code.as_str(),
                    "server.host_unavailable"
                        | "server.host_not_running"
                        | "server.network_not_running"
                ) => {}
            Err(why) => {
                instance.problem = Some(why.clone());
                if instance.state != State::Running {
                    instance.state = State::Failed;
                }
                let _ = self.save();
            }
        }
        done
    }

    /// Stop one, and remember it is not meant to run.
    fn stop_service(&mut self, at: usize) -> Result<Data, Reason> {
        self.services[at].wanted = false;
        self.services[at].problem = None;
        self.halt(at)?;
        self.save()?;
        Ok(self.moved(at))
    }

    /// Run it, if it is not running already.
    fn launch(&mut self, at: usize) -> Result<(), Reason> {
        if self.running(at)? {
            if !self.services[at].definition.is_internal() {
                self.discover_host_addresses();
            }
            return Ok(());
        }
        let instance = &self.services[at];
        let values = self.run_values(at);
        let Runtime::Process { command, directory } = &instance.definition.runtime else {
            return self.launch_internal(at);
        };
        if self.peers.id("host").is_none() {
            return Err(reason!("server.needs_host", service = instance.name));
        }
        // Said as the Server's own refusal, naming what to do: the Host's
        // engine.not_running, passed on, read as if the Server were off.
        if self
            .peers
            .state("host")
            .is_some_and(|state| state.lifecycle != Lifecycle::Running)
        {
            return Err(reason!("server.host_not_running", service = instance.name));
        }
        if let Some(state) = self.peers.state("host")
            && state.fact("family").and_then(Value::as_str) == Some("windows")
        {
            return Err(reason!("server.guest_not_linux", service = instance.name));
        }
        self.check_ports(at, &values)?;
        let argv: Vec<String> = command.iter().map(|word| fill(word, &values)).collect();
        let directory = directory
            .as_deref()
            .map_or_else(|| "/".to_owned(), |d| fill(d, &values));
        let files: Vec<(String, String)> = instance
            .definition
            .files
            .iter()
            .map(|file| (fill(&file.path, &values), fill(&file.content, &values)))
            .collect();
        let (name, id) = (instance.name.clone(), instance.id.clone());
        let secrets = instance.secrets(&self.own.secrets);
        let prepare = instance.definition.prepare.as_ref().map(|command| {
            command
                .iter()
                .map(|word| fill(word, &values))
                .collect::<Vec<_>>()
        });

        process::write_files(&self.peers, &files, &secrets)?;
        process::require_program(&self.peers, &name, &directory, &argv[0])?;
        if let Some(folder) = self.shared_tls()
            && instance.definition.uses("_tls")
            && instance.definition.answers_https(&values)
        {
            process::certificate(&self.peers, &name, &folder, &self.own.domain)?;
        }
        if let Some(command) = prepare {
            process::prepare(&self.peers, &name, &command, &secrets).map_err(|mut why| {
                for value in why.values.values_mut() {
                    *value = instance.hide(&self.own.secrets, std::mem::take(value));
                }
                // The check runs inside the machine and cannot know what it is
                // called here; "on the machine" alone reads as this computer.
                if let Some(machine) = self.peers.name("host") {
                    why.values.entry("machine".into()).or_insert(machine);
                }
                why
            })?;
        }
        let started = process::start(&self.peers, &id, &directory, &argv, &secrets)?;
        let alive = match started {
            Ok(pid) => process::alive(&self.peers, pid, &argv[0])?.then_some(pid),
            Err(_) => None,
        };
        self.discover_host_addresses();
        let address = self.address_of(false);
        let run = self.machine_run();
        let instance = &mut self.services[at];
        match alive {
            Some(pid) => {
                instance.state = State::Running;
                instance.pid = Some(pid);
                instance.seen_in = run;
                let endpoints = self.services[at].endpoints(address.as_deref());
                self.emit(
                    "service.started",
                    data!({"service": name, "pid": pid, "endpoints": endpoints}),
                );
                self.settle(at)?;
                if self.services[at].state == State::Failed {
                    let said = self.services[at].hide(
                        &self.own.secrets,
                        process::tail(&self.peers, &id, 20)
                            .unwrap_or_default()
                            .join("\n"),
                    );
                    return Err(reason!("server.did_not_start", service = name, said = said));
                }
                Ok(())
            }
            None => {
                instance.state = State::Failed;
                instance.pid = None;
                let said = instance.hide(
                    &self.own.secrets,
                    match started {
                        Err(said) => said,
                        Ok(_) => process::tail(&self.peers, &id, 20)
                            .unwrap_or_default()
                            .join("\n"),
                    },
                );
                self.emit("service.failed", data!({"service": name, "log_tail": said}));
                let _ = self.save();
                Err(reason!("server.did_not_start", service = name, said = said))
            }
        }
    }

    /// Stop it, if it is running.
    fn halt(&mut self, at: usize) -> Result<(), Reason> {
        if self.services[at].definition.is_internal() {
            let name = self.services[at].name.clone();
            let gone = self
                .internals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&name);
            if let Some(gone) = gone {
                self.services[at].memory = gone.implementation.memory();
            }
            self.publish(&name);
            self.drop_node_if_idle();
        } else if self.running(at)?
            && let Some(pid) = self.services[at].pid
        {
            process::stop(&self.peers, pid)?;
        }
        let instance = &mut self.services[at];
        let was = instance.state;
        if !instance.wanted {
            instance.pid = None;
        }
        if matches!(was, State::Running) {
            instance.state = State::Stopped;
            instance.pid = None;
            let name = instance.name.clone();
            self.emit("service.stopped", data!({"service": name}));
        }
        self.set_health(at, Wellness::Unknown);
        Ok(())
    }

    /// Whether it is really running: asked of the guest, so a service that
    /// died on its own is found here, said, and marked `failed`.
    /// Which run of the machine this is: when it last started, and when it
    /// last went back to a save point. Either can happen while nobody asks
    /// the Server, and leaves the guest without the processes of before.
    fn machine_run(&self) -> Option<String> {
        let state = self.peers.state("host")?;
        let went_back = state.fact("went_back_at").cloned().unwrap_or_default();
        Some(format!("{} {went_back}", state.since))
    }

    fn running(&mut self, at: usize) -> Result<bool, Reason> {
        let run = self.machine_run();
        let instance = &self.services[at];
        if instance.definition.is_internal() {
            // Its network off or deleted: nobody can reach it, so it is not
            // said to run. Still meant to: `revive` brings it back.
            if instance.state == State::Running && !self.network_up() {
                self.services[at].state = State::Stopped;
                self.set_health(at, Wellness::Unknown);
                let _ = self.save();
                return Ok(false);
            }
            return Ok(instance.state == State::Running && self.registered(&instance.name));
        }
        // A pid on a stopped one is kept from a machine that went off: one
        // whose state was saved comes back with that process still in it.
        let (State::Running | State::Stopped, Some(pid)) = (instance.state, instance.pid) else {
            return Ok(false);
        };
        let Runtime::Process { command, .. } = &instance.definition.runtime else {
            return Ok(false);
        };
        let was = instance.state;
        let program = fill(&command[0], &instance.values());
        match process::alive(&self.peers, pid, &program) {
            Ok(true) if was == State::Running => {
                if self.services[at].seen_in != run {
                    self.services[at].seen_in = run;
                    let _ = self.save();
                }
                return Ok(true);
            }
            Ok(true) => {
                let name = instance.name.clone();
                let address = self.address_of(false);
                self.services[at].state = State::Running;
                self.services[at].seen_in = run;
                let endpoints = self.services[at].endpoints(address.as_deref());
                self.emit(
                    "service.started",
                    data!({"service": name, "pid": pid, "endpoints": endpoints}),
                );
                let _ = self.save();
                return Ok(true);
            }
            // Gone while the machine was off: booted afresh, not failed.
            Ok(false) if was == State::Stopped => {
                self.services[at].pid = None;
                let _ = self.save();
                return Ok(false);
            }
            // Same, when nobody asked while it was off, or it went back to a
            // save point from before the service: its machine's, not its own.
            Ok(false) if instance.seen_in.is_some() && instance.seen_in != run => {
                let instance = &mut self.services[at];
                instance.state = State::Stopped;
                instance.pid = None;
                self.set_health(at, Wellness::Unknown);
                let _ = self.save();
                return Ok(false);
            }
            Ok(false) => {}
            // The machine is off: its processes went with it, or wait in its
            // saved state. Not a failure of the service, and still meant to
            // run when the machine is back. A machine deleted took them too:
            // stopped, so the service can still be listed and removed.
            Err(why)
                if matches!(
                    why.code.as_str(),
                    "server.host_unavailable" | "server.host_gone"
                ) =>
            {
                let instance = &mut self.services[at];
                instance.state = State::Stopped;
                self.set_health(at, Wellness::Unknown);
                let _ = self.save();
                return Ok(false);
            }
            Err(why) => return Err(why),
        }
        let (name, id) = (instance.name.clone(), instance.id.clone());
        let said = instance.hide(
            &self.own.secrets,
            process::tail(&self.peers, &id, 20)
                .unwrap_or_default()
                .join("\n"),
        );
        let instance = &mut self.services[at];
        instance.state = State::Failed;
        instance.pid = None;
        // Said on the page too, not only in the history: its log says why.
        instance.problem = Some(reason!("server.died", service = name));
        self.emit("service.failed", data!({"service": name, "log_tail": said}));
        self.set_health(at, Wellness::Unknown);
        let _ = self.save();
        Ok(false)
    }

    /// No two running services on one transport and port where they run.
    fn check_ports(&self, at: usize, values: &BTreeMap<String, String>) -> Result<(), Reason> {
        let internal = self.services[at].definition.is_internal();
        for wanted in &self.services[at].definition.interfaces {
            let Some(port) = wanted.port.resolve(values) else {
                if wanted.port.is_off(values) {
                    continue;
                }
                return Err(reason!("server.bad_port", service = self.services[at].name));
            };
            let by = self.services.iter().enumerate().find(|(other, each)| {
                *other != at
                    && each.state == State::Running
                    && each.definition.is_internal() == internal
                    && each.definition.interfaces.iter().any(|theirs| {
                        theirs.transport == wanted.transport
                            && theirs.port.resolve(&each.values()) == Some(port)
                    })
            });
            if let Some((_, by)) = by {
                return Err(reason!("server.port_taken", port = port, by = by.name));
            }
        }
        Ok(())
    }

    /// Do one of the service's own capabilities: its inputs checked as
    /// any action's are, then done by its runtime -- for a `process` one,
    /// its `run` argv, with the configuration and the inputs in its
    /// placeholders.
    fn call(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let instance = &self.services[at];
        let capability = text(action, "capability");
        let declared = instance
            .definition
            .capabilities
            .iter()
            .find(|c| c.capability == capability);
        let (Some(declared), Some(spec)) = (declared, instance.definition.spec(&capability)) else {
            return Err(reason!(
                "server.no_such_capability",
                service = instance.name,
                capability = capability
            ));
        };
        let arguments = action
            .argument("arguments")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        spec.check_inputs(&arguments)?;
        let Some(run) = &declared.run else {
            return self.call_internal(at, &capability, &arguments);
        };
        let mut values = self.run_values(at);
        // An optional input left out is empty, not a literal `{name}`.
        for input in &declared.inputs {
            let value = arguments.get(&input.name).filter(|v| !v.is_null());
            values.insert(
                input.name.clone(),
                value.map(crate::definition::as_text).unwrap_or_default(),
            );
        }
        let argv: Vec<String> = run.iter().map(|word| fill(word, &values)).collect();
        let said = process::execute(&self.peers, &argv, &instance.secrets(&self.own.secrets))?;
        let mut answer = data!({"service": instance.name, "capability": capability});
        for key in ["exit_code", "stdout", "stderr"] {
            if let Some(value) = said.get(key) {
                let value = match value.as_str() {
                    Some(text) => Value::from(instance.hide(&self.own.secrets, text.to_owned())),
                    None => value.clone(),
                };
                answer.insert(key.into(), value);
            }
        }
        Ok(answer)
    }

    /// An internal service's capability, done by its implementation --
    /// the running one, or, while it is stopped, one picked up from what it
    /// remembers: a DNS record can be added before the DNS is started.
    fn call_internal(
        &mut self,
        at: usize,
        capability: &str,
        arguments: &Data,
    ) -> Result<Data, Reason> {
        let instance = &self.services[at];
        let name = instance.name.clone();
        let (done, memory) = {
            let mut running = self
                .internals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match running.get_mut(&name) {
                Some(live) => {
                    let done = live.implementation.call(capability, arguments);
                    (done, live.implementation.memory())
                }
                None => {
                    let Runtime::Internal { implementation } = &instance.definition.runtime else {
                        unreachable!("call_internal is for internal services");
                    };
                    let mut resting = internal::make(implementation, &instance.memory)
                        .expect("a loaded definition names an implementation this build has");
                    let done = resting.call(capability, arguments);
                    (done, resting.memory())
                }
            }
        };
        let said = done?;
        self.services[at].memory = memory;
        self.publish(&name);
        self.save()?;
        let mut answer = data!({"service": name, "capability": capability});
        answer.extend(said);
        Ok(answer)
    }

    fn logs(&self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let instance = &self.services[at];
        if instance.definition.is_internal() {
            return Ok(data!({"service": instance.name, "lines": []}));
        }
        let lines = action
            .argument("lines")
            .and_then(Value::as_u64)
            .unwrap_or(100);
        let lines: Vec<String> = process::tail(&self.peers, &instance.id, lines)?
            .into_iter()
            .map(|line| instance.hide(&self.own.secrets, line))
            .collect();
        Ok(data!({"service": instance.name, "lines": lines}))
    }

    /// One action, and the domain's names brought in line with what it did.
    fn act(&mut self, action: &Action) -> Result<Data, Reason> {
        let done = self.act_on(action);
        self.publish_domain();
        done
    }

    fn act_on(&mut self, action: &Action) -> Result<Data, Reason> {
        match action.capability() {
            "service.catalog" => {
                let (all, refused) = self.catalog();
                let definitions: Vec<Value> = all
                    .iter()
                    .map(|(d, source)| {
                        json!({
                            "id": d.id, "version": d.version, "name": d.name,
                            "description": d.description, "author": d.author, "source": source,
                            "runtime": if d.is_internal() { "internal" } else { "process" },
                            "configuration": d.configuration, "interfaces": d.interfaces,
                            "runs": d.runs(),
                            "requires": d.requires,
                        })
                    })
                    .collect();
                Ok(data!({"definitions": definitions, "refused": refused}))
            }
            "service.create" => self.create_service(action),
            "service.configure" => self.configure_service(action),
            "service.remove" => self.remove_service(action),
            "service.start" => {
                let at = self.find(&text(action, "service"))?;
                self.start_service(at)
            }
            "service.stop" => {
                let at = self.find(&text(action, "service"))?;
                self.stop_service(at)
            }
            "service.restart" => {
                let at = self.find(&text(action, "service"))?;
                self.halt(at)?;
                self.start_service(at)
            }
            "service.list" => {
                for at in 0..self.services.len() {
                    if self.running(at)?
                        && action.argument("check_health").and_then(Value::as_bool) == Some(true)
                    {
                        self.check_health(at)?;
                    }
                }
                self.revive();
                if self
                    .services
                    .iter()
                    .any(|each| each.state == State::Running && !each.definition.is_internal())
                {
                    self.discover_host_addresses();
                }
                let services: Vec<Value> = self
                    .services
                    .iter()
                    .map(|each| {
                        each.summary(self.address_of(each.definition.is_internal()).as_deref())
                    })
                    .collect();
                Ok(data!({"services": services}))
            }
            "service.logs" => self.logs(action),
            "service.health" => {
                let at = self.find(&text(action, "service"))?;
                self.check_health(at)?;
                self.save()?;
                Ok(self.moved(at))
            }
            "service.describe" => {
                let at = self.find(&text(action, "service"))?;
                let instance = &self.services[at];
                let definition = &instance.definition;
                Ok(data!({
                    "service": instance.name,
                    "definition": definition,
                    "config": definition.masked(&instance.config),
                    "capabilities": definition.capabilities,
                    "data": instance.data(),
                }))
            }
            "service.call" => self.call(action),
            "service.define" => self.define(action),
            "server.settings" => self.settings(action),
            "service.depend" => self.depend(action),
            "service.supervise" => self.supervise(),
            "server.check" => self.check(action),
            "server.secret.set" => self.set_secret(action),
            "server.secret.remove" => self.remove_secret(action),
            other => Err(reason!("engine.unsupported", capability = other)),
        }
    }
}

/// Where the services are written down.
const SERVICES: &str = "services.json";
/// Where the Server's own settings are.
const OWN: &str = "own.json";

/// A name for a domain: dot-separated labels of lowercase letters, digits
/// and `-`, neither starting nor ending with `-`.
fn is_domain(text: &str) -> bool {
    text.len() <= 200
        && text.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// A list of names an action was given, as text; none when not given.
fn words(action: &Action, key: &str) -> Vec<String> {
    match action.argument(key) {
        Some(Value::Array(items)) => items.iter().map(crate::definition::as_text).collect(),
        _ => Vec::new(),
    }
}

fn text(action: &Action, key: &str) -> String {
    action
        .argument(key)
        .map(crate::definition::as_text)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Written readable by its owner only: it holds the secrets.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

impl ServerSandbox {
    /// [`Self::supervise`] for the daemon's own thread, reached through
    /// `Sandbox::provider` so that a look that finds nothing to do says
    /// nothing on the bus: the names it started again.
    pub fn supervise_now(&mut self) -> Vec<String> {
        self.supervise()
            .ok()
            .and_then(|said| said.get("restarted").cloned())
            .and_then(|names| serde_json::from_value(names).ok())
            .unwrap_or_default()
    }
}

impl SandboxProvider for ServerSandbox {
    fn given(&mut self, peers: Peers) {
        self.peers = peers;
        self.host_addresses = None;
    }

    fn create(&mut self) -> Result<(), ProviderError> {
        std::fs::create_dir_all(&self.room).map_err(|error| {
            ProviderError::new(reason!("server.could_not_save", detail = error))
        })?;
        self.save().map_err(ProviderError::new)
    }

    /// Every service meant to run, started. One that will not start is
    /// marked `failed` and said; the Server itself still starts.
    fn start(&mut self) -> Result<(), ProviderError> {
        self.own.on = Some(true);
        self.save_own().map_err(ProviderError::new)?;
        for at in self.order() {
            if !(self.own.restore && self.services[at].wanted) {
                continue;
            }
            if let Some(need) = self.needs_running(at) {
                let instance = &mut self.services[at];
                instance.problem = Some(reason!(
                    "server.after_failed",
                    service = instance.name,
                    after = need
                ));
                instance.state = State::Failed;
                continue;
            }
            if let Err(why) = self.try_launch(at) {
                log::warn!(
                    "{} could not start {}: {why:?}",
                    self.identity.id,
                    self.services[at].name
                );
            }
        }
        self.publish_domain();
        self.save().map_err(ProviderError::new)
    }

    /// Every service stopped, each still meant to run when the Server
    /// starts again. A Host that cannot be asked any more leaves nothing to
    /// stop: its services went with it.
    fn stop(&mut self) -> Result<(), ProviderError> {
        self.own.on = Some(false);
        self.save_own().map_err(ProviderError::new)?;
        for at in 0..self.services.len() {
            if let Err(why) = self.halt(at) {
                log::warn!(
                    "{} could not stop {}: {why:?}",
                    self.identity.id,
                    self.services[at].name
                );
                let instance = &mut self.services[at];
                if instance.state == State::Running {
                    instance.state = State::Stopped;
                    instance.pid = None;
                }
                self.set_health(at, Wellness::Unknown);
            }
        }
        self.publish_domain();
        self.save().map_err(ProviderError::new)
    }

    fn destroy(&mut self) -> Result<(), ProviderError> {
        let _ = self.stop();
        match std::fs::remove_dir_all(&self.room) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(ProviderError::new(
                reason!("server.could_not_save", detail = error),
            )),
            _ => Ok(()),
        }
    }

    /// Every service stopped and forgotten.
    fn reset(&mut self) -> Result<(), ProviderError> {
        self.stop()?;
        let gone: Vec<Instance> = std::mem::take(&mut self.services);
        for instance in gone {
            self.emit(
                "service.removed",
                data!({"service": instance.name, "definition": instance.definition.id}),
            );
        }
        self.save().map_err(ProviderError::new)
    }

    /// What was last seen, without asking any guest: this runs after every
    /// transition and has to be cheap.
    fn observe(&mut self) -> Observation {
        self.revive();
        let services: Vec<Value> = self
            .services
            .iter()
            .map(|each| {
                json!({
                    "service": each.name,
                    "state": each.state.as_str(),
                    "health": each.health.as_str(),
                })
            })
            .collect();
        let endpoints: Vec<Value> = self
            .services
            .iter()
            .flat_map(|each| {
                each.endpoints(self.address_of(each.definition.is_internal()).as_deref())
            })
            .collect();
        Observation {
            alive: Some(self.room.is_dir()),
            facts: data!({"services": services, "endpoints": endpoints}),
            ..Observation::default()
        }
    }

    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        self.act(action).map_err(ProviderError::new)
    }
}

/// `folder` as `~/…` when it is under `home`. The page shows this one, so a
/// screenshot doesn't carry the user's name; copying still gives the full path.
fn from_home(folder: &Path, home: Option<PathBuf>) -> String {
    match home
        .as_deref()
        .and_then(|home| folder.strip_prefix(home).ok())
    {
        Some(rest) => Path::new("~").join(rest).to_string_lossy().into_owned(),
        None => folder.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_under_home_is_shown_from_the_tilde() {
        let home = Some(PathBuf::from("/home/ana"));
        // `~\kinds` on Windows: the system's own separator.
        assert_eq!(
            from_home(Path::new("/home/ana/kinds"), home.clone()),
            Path::new("~").join("kinds").to_string_lossy()
        );
        assert_eq!(
            from_home(Path::new("/home/anabela/kinds"), home.clone()),
            "/home/anabela/kinds"
        );
        assert_eq!(from_home(Path::new("/srv/kinds"), home), "/srv/kinds");
        assert_eq!(from_home(Path::new("/srv/kinds"), None), "/srv/kinds");
    }
}
