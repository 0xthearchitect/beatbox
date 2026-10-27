//! A sandbox that is not one, so the contract can be proved before a real one.
//!
//! Nothing here runs or touches the host: a map of files, some pretend
//! processes and a few named copies of both.
//!
//! It declares `none` isolation on purpose, so a configuration asking for a
//! machine can never quietly land here. Its numbers are made up (a quarter
//! core per process). `settings.refuse` and `settings.fatal` make lifecycle
//! verbs fail, and [`MockSandbox::crash`] kills it between two calls.
//!
//! Settings are read with Python's truthiness and `str()`, because
//! `golden/engine/` and `golden/api/` were recorded that way.

use std::collections::{BTreeMap, BTreeSet};

use beatbox_reasons::reason;
use serde_json::{Value, json};

use crate::events::Data;
use crate::faults::ProviderError;
use crate::runtime::actions::Action;
use crate::runtime::config::SandboxConfig;
use crate::runtime::identity::SandboxIdentity;
use crate::runtime::isolation::Isolation;
use crate::runtime::provider::{Emitter, ProviderSpec, SandboxProvider, unsupported};
use crate::runtime::resources::{CPU, MEMORY, NOTHING, Resources};
use crate::runtime::sandbox::json_data;
use crate::runtime::state::Observation;

/// What a process killed by a stop reports, the way a real one would.
pub const KILLED: i64 = 137;

/// Everything this type can ever do.
pub const CAPABILITIES: &[&str] = &[
    "lifecycle.pause",
    "lifecycle.reset",
    "lifecycle.reconfigure",
    "process.execute",
    "process.list",
    "filesystem.read",
    "filesystem.write",
    "filesystem.list",
    "snapshot.create",
    "snapshot.restore",
];

#[derive(Debug, Clone)]
struct Process {
    pid: i64,
    command: String,
    running: bool,
}

type Snapshot = (BTreeMap<String, String>, Vec<Process>, i64);

/// Everything a sandbox can do, done in a map.
///
/// Configured entirely through `settings`:
///
/// | | |
/// |---|---|
/// | `files` | `{path: content}` the sandbox is created with, and returns to when it is reset. |
/// | `refuse` | lifecycle verbs to fail on: `["start"]`, `["pause"]`. |
/// | `fatal` | whether those refusals lose the sandbox rather than just failing one call. |
pub struct MockSandbox {
    emit: Emitter,
    config: SandboxConfig,
    initial: BTreeMap<String, String>,
    refuse: BTreeSet<String>,
    fatal: bool,
    files: BTreeMap<String, String>,
    processes: Vec<Process>,
    snapshots: BTreeMap<String, Snapshot>,
    next_pid: i64,
    running: bool,
    paused: bool,
    crashed: bool,
    destroyed: bool,
}

impl MockSandbox {
    /// `mock`, version 1, isolating nothing.
    pub fn spec() -> ProviderSpec {
        ProviderSpec::new(
            "mock",
            "1",
            CAPABILITIES,
            Isolation::None,
            |identity, config, emit| Box::new(Self::new(&identity, config, emit)),
        )
    }

    fn new(_identity: &SandboxIdentity, config: SandboxConfig, emit: Emitter) -> Self {
        let initial = config
            .setting("files")
            .and_then(Value::as_object)
            .map(|files| {
                files
                    .iter()
                    .map(|(path, content)| (path.clone(), py_str(content)))
                    .collect()
            })
            .unwrap_or_default();
        let refuse = config
            .setting("refuse")
            .and_then(Value::as_array)
            .map(|steps| steps.iter().map(py_str).collect())
            .unwrap_or_default();
        let fatal = config.setting("fatal").is_some_and(truthy);
        Self {
            emit,
            config,
            initial,
            refuse,
            fatal,
            files: BTreeMap::new(),
            processes: Vec::new(),
            snapshots: BTreeMap::new(),
            next_pid: 1,
            running: false,
            paused: false,
            crashed: false,
            destroyed: false,
        }
    }

    /// Die the way the world kills things: quietly, between two calls.
    ///
    /// Nothing is emitted; the next observation finds it, as with a real guest.
    pub fn crash(&mut self) {
        self.crashed = true;
        self.running = false;
        for process in &mut self.processes {
            process.running = false;
        }
    }

    /// Undo a crash, for the test that a dead sandbox never comes back to
    /// life on its own.
    pub fn uncrash(&mut self) {
        self.crashed = false;
    }

    /// For the tests that prove what it may not emit.
    pub fn emitter(&self) -> &Emitter {
        &self.emit
    }

    /// The configuration this provider was last given.
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    fn say(&self, name: &str, data: Data) {
        self.emit
            .emit(name, data)
            .expect("the mock emits valid names outside sandbox.*");
    }

    fn refuse_if_told(&self, step: &str) -> Result<(), ProviderError> {
        if self.refuse.contains(step) {
            let why = reason!("engine.mock_refused", step = step);
            return Err(ProviderError {
                reason: Some(why),
                fatal: self.fatal,
            });
        }
        Ok(())
    }

    fn spawn(&mut self, command: &str) -> usize {
        let process = Process {
            pid: self.next_pid,
            command: command.to_owned(),
            running: true,
        };
        self.next_pid += 1;
        let pid = process.pid;
        self.processes.push(process);
        self.say(
            "process.created",
            json_data(&[("pid", json!(pid)), ("command", json!(command))]),
        );
        self.processes.len() - 1
    }

    fn reap(&mut self, index: usize, exit_code: i64) {
        let process = &mut self.processes[index];
        if !process.running {
            return;
        }
        process.running = false;
        let (pid, command) = (process.pid, process.command.clone());
        self.say(
            "process.exited",
            json_data(&[
                ("pid", json!(pid)),
                ("command", json!(command)),
                ("exit_code", json!(exit_code)),
            ]),
        );
    }

    fn require_running(&self) -> Result<(), ProviderError> {
        if !self.running || self.paused {
            return Err(ProviderError::new(reason!("engine.not_running")));
        }
        Ok(())
    }

    fn required(action: &Action, name: &str) -> Result<String, ProviderError> {
        match action.argument(name) {
            None | Some(Value::Null) => Err(Self::missing(action, name)),
            Some(Value::String(text)) if text.is_empty() => Err(Self::missing(action, name)),
            Some(value) => Ok(py_str(value)),
        }
    }

    fn missing(action: &Action, name: &str) -> ProviderError {
        ProviderError::new(reason!(
            "engine.missing_argument",
            capability = action.capability(),
            argument = name,
        ))
    }

    fn execute(&mut self, action: &Action) -> Result<Data, ProviderError> {
        self.require_running()?;
        let command = Self::required(action, "command")?;
        let linger = action.argument("linger").is_some_and(truthy);
        let exit_code = action.argument("exit_code").map_or(0, py_int);
        let index = self.spawn(&command);
        if !linger {
            self.reap(index, exit_code);
        }
        let pid = self.processes[index].pid;
        Ok(json_data(&[
            ("pid", json!(pid)),
            ("command", json!(command)),
            (
                "exit_code",
                if linger {
                    Value::Null
                } else {
                    json!(exit_code)
                },
            ),
        ]))
    }

    fn list_processes(&self) -> Data {
        let running: Vec<Value> = self
            .processes
            .iter()
            .filter(|process| process.running)
            .map(|process| json!({"pid": process.pid, "command": process.command}))
            .collect();
        json_data(&[("processes", Value::Array(running))])
    }

    fn read(&self, action: &Action) -> Result<Data, ProviderError> {
        let path = Self::required(action, "path")?;
        match self.files.get(&path) {
            Some(content) => Ok(json_data(&[
                ("path", json!(path)),
                ("content", json!(content)),
            ])),
            None => Err(ProviderError::new(reason!(
                "engine.no_such_file",
                path = path
            ))),
        }
    }

    fn write(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let path = Self::required(action, "path")?;
        let content = action.argument("content").map(py_str).unwrap_or_default();
        let existed = self.files.contains_key(&path);
        let bytes = content.chars().count();
        self.files.insert(path.clone(), content);
        self.say(
            if existed {
                "file.modified"
            } else {
                "file.created"
            },
            json_data(&[("path", json!(path)), ("bytes", json!(bytes))]),
        );
        Ok(json_data(&[("path", json!(path)), ("bytes", json!(bytes))]))
    }

    fn list_files(&self) -> Data {
        json_data(&[("paths", json!(self.files.keys().collect::<Vec<_>>()))])
    }

    fn snapshot(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let name = Self::required(action, "name")?;
        let taken = (self.files.clone(), self.processes.clone(), self.next_pid);
        self.snapshots.insert(name.clone(), taken);
        let files = self.files.len();
        self.say(
            "snapshot.created",
            json_data(&[("name", json!(name)), ("files", json!(files))]),
        );
        Ok(json_data(&[("name", json!(name)), ("files", json!(files))]))
    }

    fn restore(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let name = Self::required(action, "name")?;
        let Some((files, processes, next_pid)) = self.snapshots.get(&name).cloned() else {
            return Err(ProviderError::new(reason!(
                "engine.no_such_snapshot",
                name = name
            )));
        };
        self.files = files;
        self.processes = processes;
        self.next_pid = next_pid;
        let files = self.files.len();
        self.say(
            "snapshot.restored",
            json_data(&[("name", json!(name)), ("files", json!(files))]),
        );
        Ok(json_data(&[("name", json!(name)), ("files", json!(files))]))
    }
}

impl SandboxProvider for MockSandbox {
    fn create(&mut self) -> Result<(), ProviderError> {
        self.refuse_if_told("create")?;
        self.files = self.initial.clone();
        self.processes.clear();
        self.snapshots.clear();
        self.next_pid = 1;
        Ok(())
    }

    fn start(&mut self) -> Result<(), ProviderError> {
        self.refuse_if_told("start")?;
        self.running = true;
        self.paused = false;
        // So `process.list` has something and stop has something to kill.
        self.spawn("init");
        Ok(())
    }

    fn stop(&mut self) -> Result<(), ProviderError> {
        self.refuse_if_told("stop")?;
        for index in 0..self.processes.len() {
            self.reap(index, KILLED);
        }
        self.running = false;
        self.paused = false;
        Ok(())
    }

    fn destroy(&mut self) -> Result<(), ProviderError> {
        // No refusal hook: a provider that can refuse destroy leaks.
        self.files.clear();
        self.processes.clear();
        self.snapshots.clear();
        self.running = false;
        self.paused = false;
        self.destroyed = true;
        Ok(())
    }

    fn observe(&mut self) -> Observation {
        let alive = !(self.crashed || self.destroyed);
        let running: Vec<&Process> = self.processes.iter().filter(|p| p.running).collect();
        let count = running.len() as f64;
        let resources = if running.is_empty() {
            NOTHING
        } else {
            Resources::new([(CPU, 0.25 * count), (MEMORY, 64.0 * count)]).expect("positive amounts")
        };
        let facts = json_data(&[
            ("running", json!(self.running && !self.paused)),
            (
                "processes",
                json!(running.iter().map(|p| &p.command).collect::<Vec<_>>()),
            ),
            ("files", json!(self.files.len())),
            (
                "snapshots",
                json!(self.snapshots.keys().collect::<Vec<_>>()),
            ),
        ]);
        Observation {
            alive: Some(alive),
            halted: false,
            resources,
            facts,
            reason: (!alive).then(|| reason!("engine.sandbox_lost", type = "mock")),
        }
    }

    fn reconfigure(&mut self, config: &SandboxConfig) -> Result<(), ProviderError> {
        self.refuse_if_told("reconfigure")?;
        self.config = config.clone();
        Ok(())
    }

    fn pause(&mut self) -> Result<(), ProviderError> {
        self.refuse_if_told("pause")?;
        self.paused = true;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), ProviderError> {
        self.refuse_if_told("resume")?;
        self.paused = false;
        Ok(())
    }

    fn reset(&mut self) -> Result<(), ProviderError> {
        self.refuse_if_told("reset")?;
        self.destroy()?;
        self.destroyed = false;
        self.crashed = false;
        self.create()
    }

    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        match action.capability() {
            "process.execute" => self.execute(action),
            "process.list" => Ok(self.list_processes()),
            "filesystem.read" => self.read(action),
            "filesystem.write" => self.write(action),
            "filesystem.list" => Ok(self.list_files()),
            "snapshot.create" => self.snapshot(action),
            "snapshot.restore" => self.restore(action),
            other => Err(unsupported(other)),
        }
    }
}

/// Python's `str()`, for the scalars a setting or an argument holds.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => "None".to_owned(),
        other => other.to_string(),
    }
}

/// Python's `bool()`.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python's `int()`, for what an exit code can plausibly arrive as.
fn py_int(value: &Value) -> i64 {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        Value::Bool(flag) => i64::from(*flag),
        Value::String(text) => text
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("invalid literal for int() with base 10: {text:?}")),
        other => panic!("int() argument must be a string or a number, not {other}"),
    }
}
