//! Watchers: a line run again and again inside a machine, and told about
//! when its answer is news.
//!
//! In the daemon, so they keep watching with the window closed, and tell on
//! the bus (`watcher.changed`). Run through `process.shell`, so nothing is
//! installed inside, and only while the machine runs: off costs nothing.
//!
//! One thread for all of them, one line at a time.
//! ponytail: a slow line delays the others; a thread per machine if a lab
//! ever watches enough for that to show.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use beatbox_contract::WatcherView;
use beatbox_engine::{Lifecycle, SandboxRuntime, data};
use serde_json::{Map, Value};

use crate::commands::{new_id, read_list, write_list};
use crate::fault::ApiFault;
use crate::{Api, py};

/// Where the watchers are kept, under BeatBox's root.
pub const FILE: &str = "watchers.json";
/// How often, unless said.
pub const EVERY: i64 = 30;
/// The most often anything is run: a tighter loop is a machine kept busy.
pub const EVERY_MIN: i64 = 5;
/// What `tell` may be: when the answer changes, when it fails, when it holds a text.
pub const TELLS: [&str; 3] = ["change", "fails", "contains"];
/// How much of an answer is kept, and how much goes into an event.
const KEPT: usize = 4000;
const SAID: usize = 1000;

/// What one watcher remembers between two runs.
#[derive(Default)]
struct Memory {
    due: Option<Instant>,
    last: Option<String>,
    failing: bool,
    matching: bool,
}

/// Every watcher, and the thread that runs them.
pub struct Watchers {
    path: PathBuf,
    list: Mutex<Vec<WatcherView>>,
    memory: Mutex<HashMap<String, Memory>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn clip(text: &str, most: usize) -> String {
    match text.char_indices().nth(most) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

impl Watchers {
    /// The ones kept in `path`, none of them run yet.
    pub fn open(path: PathBuf) -> Arc<Self> {
        let mut list: Vec<WatcherView> = read_list(&path);
        for watcher in &mut list {
            watcher.state = if watcher.paused { "paused" } else { "waiting" }.into();
        }
        Arc::new(Self {
            path,
            list: Mutex::new(list),
            memory: Mutex::new(HashMap::new()),
        })
    }

    /// Every watcher, as it is now.
    pub fn list(&self) -> Vec<WatcherView> {
        lock(&self.list).clone()
    }

    fn keep(&self, list: &[WatcherView]) -> Result<(), ApiFault> {
        write_list(&self.path, list).map_err(|error| {
            ApiFault::new("internal", format!("could not keep the watchers: {error}"))
        })
    }

    /// Start one, or change one: its line, how often, when it tells, paused.
    pub fn save(&self, raw: &Map<String, Value>) -> Result<Vec<WatcherView>, ApiFault> {
        let field = |key: &str| {
            raw.get(key)
                .map(py::text_or_empty)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let bad = |message: &str| ApiFault::new("bad-payload", message);
        let tell = Some(field("tell"))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "change".into());
        if !TELLS.contains(&tell.as_str()) {
            return Err(bad("A watcher tells on change, fails or contains."));
        }
        let text = field("text");
        if tell == "contains" && text.is_empty() {
            return Err(bad(
                "A watcher that tells when a text appears needs the text.",
            ));
        }
        let every = raw
            .get("every")
            .and_then(Value::as_i64)
            .unwrap_or(EVERY)
            .max(EVERY_MIN);
        let paused = raw.get("paused").is_some_and(py::truthy);
        let id = field("id");
        let mut list = lock(&self.list);
        match list.iter_mut().find(|w| !id.is_empty() && w.id == id) {
            Some(watcher) => {
                for (key, slot) in [("name", &mut watcher.name), ("line", &mut watcher.line)] {
                    if raw.contains_key(key) {
                        *slot = field(key);
                    }
                }
                if raw.contains_key("tell") {
                    watcher.tell = tell;
                    watcher.text = text;
                }
                if raw.contains_key("every") {
                    watcher.every = every;
                }
                if raw.contains_key("paused") {
                    watcher.paused = paused;
                    watcher.state = if paused { "paused" } else { "waiting" }.into();
                }
                // Changed: judged afresh, from its next answer.
                lock(&self.memory).remove(&watcher.id);
            }
            None => {
                let (name, line, sandbox_id) = (field("name"), field("line"), field("sandboxId"));
                if line.is_empty() || sandbox_id.is_empty() {
                    return Err(bad("A watcher needs a machine and a line to run."));
                }
                list.push(WatcherView {
                    id: new_id("wat"),
                    name: if name.is_empty() { line.clone() } else { name },
                    sandbox_id,
                    line,
                    every,
                    tell,
                    text,
                    paused,
                    state: if paused { "paused" } else { "waiting" }.into(),
                    runs: 0,
                    last_at: String::new(),
                    last_exit: None,
                    last_output: String::new(),
                    last_reason: None,
                    changed_at: String::new(),
                });
            }
        }
        self.keep(&list)?;
        Ok(list.clone())
    }

    /// Stop and forget one.
    pub fn forget(&self, id: &str) -> Result<Vec<WatcherView>, ApiFault> {
        let mut list = lock(&self.list);
        list.retain(|watcher| watcher.id != id);
        lock(&self.memory).remove(id);
        self.keep(&list)?;
        Ok(list.clone())
    }

    /// Run them, for as long as the runtime is there.
    pub fn watch(self: &Arc<Self>, runtime: Weak<SandboxRuntime>) {
        let watchers = Arc::clone(self);
        thread::Builder::new()
            .name("beatboxd-watchers".into())
            .spawn(move || {
                while let Some(runtime) = runtime.upgrade() {
                    watchers.tick(&runtime);
                    drop(runtime);
                    thread::sleep(Duration::from_secs(1));
                }
            })
            .expect("a thread can be started");
    }

    /// Run every watcher that is due, one at a time.
    fn tick(&self, runtime: &SandboxRuntime) {
        let now = Instant::now();
        // `list` before `memory`, always, and never both held here: `save`
        // takes them in that order, and the other order is a deadlock.
        let all = lock(&self.list).clone();
        let due: Vec<WatcherView> = {
            let memory = lock(&self.memory);
            all.into_iter()
                .filter(|w| {
                    !w.paused
                        && memory
                            .get(&w.id)
                            .and_then(|m| m.due)
                            .is_none_or(|at| at <= now)
                })
                .collect()
        };
        for watcher in due {
            let every =
                Duration::from_secs(u64::try_from(watcher.every.max(EVERY_MIN)).unwrap_or(30));
            lock(&self.memory)
                .entry(watcher.id.clone())
                .or_default()
                .due = Some(now + every);
            let state = match runtime.get(&watcher.sandbox_id) {
                Err(_) => Some("gone"),
                Ok(sandbox) if sandbox.lifecycle() == Lifecycle::Destroyed => Some("gone"),
                Ok(sandbox) if sandbox.lifecycle() != Lifecycle::Running => Some("waiting"),
                Ok(sandbox) if !sandbox.capabilities().supports("process.shell") => {
                    Some("no_agent")
                }
                Ok(sandbox) => {
                    self.run(runtime, &sandbox, &watcher);
                    None
                }
            };
            if let Some(state) = state {
                self.update(&watcher.id, |w| w.state = state.into());
            }
        }
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut WatcherView)) {
        if let Some(watcher) = lock(&self.list).iter_mut().find(|w| w.id == id) {
            change(watcher);
        }
    }

    fn run(
        &self,
        runtime: &SandboxRuntime,
        sandbox: &beatbox_engine::Sandbox,
        watcher: &WatcherView,
    ) {
        let timeout = watcher.every.clamp(1, 60);
        let mut refused = None;
        let (output, exit) = match sandbox.act(
            "process.shell",
            data!({"line": watcher.line, "timeout": timeout}),
        ) {
            Ok(outcome) if outcome.ok => {
                let stdout = outcome
                    .data
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let stderr = outcome
                    .data
                    .get("stderr")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let output = if stdout.is_empty() { stderr } else { stdout };
                (
                    output.to_owned(),
                    outcome.data.get("exit_code").and_then(Value::as_i64),
                )
            }
            Ok(outcome) => {
                // Refused, not run: the reason is what the page says, in its words.
                refused = outcome.reason.as_ref().map(crate::sandbox::reason_view);
                (String::new(), None)
            }
            Err(error) => (format!("<{error}>"), None),
        };
        let failed = exit != Some(0);
        let because = {
            let mut memory = lock(&self.memory);
            let seen = memory.entry(watcher.id.clone()).or_default();
            let matches = watcher.tell == "contains" && output.contains(&watcher.text);
            let because = match watcher.tell.as_str() {
                "change" => seen
                    .last
                    .as_ref()
                    .is_some_and(|last| *last != output)
                    .then_some("changed"),
                "fails" => (failed && !seen.failing).then_some("failed"),
                _ => (matches && !seen.matching).then_some("matched"),
            };
            seen.last = Some(output.clone());
            seen.failing = failed;
            seen.matching = matches;
            because
        };
        let at = beatbox_engine::clock::now();
        self.update(&watcher.id, |w| {
            w.runs += 1;
            w.last_at.clone_from(&at);
            w.last_exit = exit;
            w.last_output = clip(&output, KEPT);
            w.last_reason.clone_from(&refused);
            w.state = if failed { "failing" } else { "watching" }.into();
            if because.is_some() {
                w.changed_at.clone_from(&at);
            }
        });
        if let Some(because) = because {
            let told = runtime.events().publish(
                "watcher.changed",
                &watcher.sandbox_id,
                data!({
                    "watcher": watcher.id,
                    "name": watcher.name,
                    "because": because,
                    "exit_code": exit,
                    "output": clip(&output, SAID),
                }),
            );
            if let Err(error) = told {
                log::warn!("a watcher could not say what changed: {error}");
            }
        }
    }
}

impl Api {
    pub(crate) fn watchers_list(&self) -> Result<Value, ApiFault> {
        Ok(serde_json::to_value(self.watchers().list()).expect("JSON"))
    }

    pub(crate) fn watchers_save(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let Some(Value::Object(raw)) = payload.get("watcher") else {
            return Err(ApiFault::new("bad-payload", "A watcher is an object."));
        };
        Ok(serde_json::to_value(self.watchers().save(raw)?).expect("JSON"))
    }

    pub(crate) fn watchers_forget(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let id = payload.get("id").map(py::text_or_empty).unwrap_or_default();
        Ok(serde_json::to_value(self.watchers().forget(&id)?).expect("JSON"))
    }
}
