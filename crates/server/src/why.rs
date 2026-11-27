//! Why a service does not answer, measured when asked, and what happened
//! to it, in order.
//!
//! The diary is the Server's own: the engine's event history is short and
//! in memory, so it forgets across a restart and fills up with every log
//! read. One JSON object per line in `diary.jsonl`, cut back as it grows.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use beatbox_engine::{Action, Data, Reason, data};
use serde_json::{Value, json};

use super::{ServerSandbox, State, Wellness, text};
use crate::definition::{Health, Transport};
use crate::placeholders::fill;
use crate::process;

/// In the Server's room.
const DIARY: &str = "diary.jsonl";

/// Lines kept when the diary is cut back. It is cut once it passes twice
/// this, so it is rewritten rarely.
const KEEP: usize = 200;

/// The most a page is given at once.
const SHOWN: usize = 60;

/// The service events the diary keeps. Calls, copies and the rest are what a
/// person did, not what happened to it.
const KEPT: [&str; 7] = [
    "service.created",
    "service.started",
    "service.stopped",
    "service.failed",
    "service.restarting",
    "service.health.changed",
    "service.removed",
];

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis().try_into().unwrap_or(u64::MAX))
}

/// One line at the end of the diary in `room`, cut back when it is long.
/// Best effort: a diary that cannot be written loses a line, nothing else.
pub(super) fn write_line(room: &Path, mut line: Data) {
    line.insert("at".into(), now_ms().into());
    let path = room.join(DIARY);
    let written = std::fs::create_dir_all(room).and_then(|()| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{}", Value::Object(line))
    });
    if let Err(error) = written {
        log::warn!("could not write {}: {error}", path.display());
        return;
    }
    let lines = read_lines(room);
    if lines.len() > 2 * KEEP {
        let kept: Vec<String> = lines[lines.len() - KEEP..]
            .iter()
            .map(|line| Value::Object(line.clone()).to_string() + "\n")
            .collect();
        let fresh = path.with_extension("jsonl.new");
        if std::fs::write(&fresh, kept.concat()).is_ok() {
            let _ = std::fs::rename(&fresh, &path);
        }
    }
}

/// Every line of the diary in `room` that reads, oldest first.
pub(super) fn read_lines(room: &Path) -> Vec<Data> {
    std::fs::read_to_string(room.join(DIARY))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| match serde_json::from_str(line) {
            Ok(Value::Object(object)) => Some(object),
            _ => None,
        })
        .collect()
}

/// The lifecycle each peer role was last written down with, so a Server
/// brought back after a restart does not write it down again.
pub(super) fn peers_seen(room: &Path) -> BTreeMap<String, String> {
    read_lines(room)
        .into_iter()
        .filter(|line| line.get("name").and_then(Value::as_str) == Some("peer"))
        .map(|line| (text_of(&line, "role"), text_of(&line, "state")))
        .collect()
}

fn text_of(line: &Data, key: &str) -> String {
    line.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// What a check that answers JSON says is missing: its keys set to `false`
/// (`{"database": false}` is "database: no"). Anything else says nothing.
fn missing_in(body: &str) -> Vec<String> {
    match serde_json::from_str(body.trim()) {
        Ok(Value::Object(object)) => object
            .into_iter()
            .filter(|(_, value)| *value == Value::Bool(false))
            .map(|(key, _)| key)
            .collect(),
        _ => Vec::new(),
    }
}

/// The definition a check's word most likely means.
// ponytail: a fixed list of words, enough for the built-ins; a definition
// could say what it is called in other programs' checks if this falls short.
fn meant(word: &str) -> Option<&'static str> {
    match word.to_ascii_lowercase().as_str() {
        "database" | "db" | "postgres" | "postgresql" | "pg" => Some("postgresql"),
        "email" | "mail" | "smtp" | "mailer" => Some("smtp"),
        "dns" | "names" | "resolver" => Some("dns"),
        _ => None,
    }
}

impl ServerSandbox {
    /// Write a service event down, when it is one the diary keeps.
    pub(super) fn note(&self, name: &str, data: &Data) {
        if KEPT.contains(&name) {
            let mut line = data.clone();
            line.insert("name".into(), name.into());
            write_line(&self.room, line);
        }
    }

    /// Write down a machine or network that changed since it was last seen.
    /// Its time is when the Server noticed: on the next action, within the
    /// page's few seconds.
    pub(super) fn note_peers(&mut self) {
        for role in ["host", "network"] {
            let Some(state) = self.peers.state(role) else {
                continue;
            };
            let now = state.lifecycle.as_str().to_owned();
            if self.seen.get(role) == Some(&now) {
                continue;
            }
            // Its id: the page knows its name, and a rename does not
            // leave the diary talking about one that is gone. `first`: it
            // was already like this, it did not just change.
            let id = self.peers.id(role).unwrap_or_default().to_owned();
            let first = !self.seen.contains_key(role);
            write_line(
                &self.room,
                data!({"name": "peer", "role": role, "sandbox": id, "state": now, "first": first}),
            );
            self.seen.insert(role.to_owned(), now);
        }
    }

    /// The diary, oldest first. A service that stops answering again after a
    /// restart says so as `again`, not as news.
    pub(super) fn diary(&self) -> Data {
        let mut last: BTreeMap<String, String> = BTreeMap::new();
        let lines: Vec<Value> = read_lines(&self.room)
            .into_iter()
            .map(|mut line| {
                if line.get("name").and_then(Value::as_str) == Some("service.health.changed") {
                    let to = text_of(&line, "to");
                    if to != "unknown" {
                        let before = last.insert(text_of(&line, "service"), to.clone());
                        line.insert("again".into(), (before.as_deref() == Some(&*to)).into());
                    }
                }
                Value::Object(line)
            })
            .collect();
        let shown = lines[lines.len().saturating_sub(SHOWN)..].to_vec();
        data!({"entries": shown})
    }

    /// Why `service` does not answer: what was seen just now, and the most
    /// likely reason. Everything is measured here; nothing is changed.
    pub(super) fn why(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let name = self.services[at].name.clone();
        let internal = self.services[at].definition.is_internal();
        let mut facts: Vec<Value> = Vec::new();
        let answer = |facts: Vec<Value>, maybe: Value| data!({"service": name.as_str(), "checked_at": now_ms(), "facts": facts, "maybe": maybe});

        if !internal {
            let up = self.host_up();
            facts.push(json!({"fact": "machine", "ok": up}));
            if !up {
                return Ok(answer(facts, json!({"kind": "machine_off"})));
            }
        }
        let running = self.running(at)?;
        facts.push(json!({"fact": "running", "ok": running}));

        let mut maybe = Value::Null;
        let mut missing = Vec::new();
        let mut healthy = false;
        if running {
            let values = self.run_values(at);
            let definition = self.services[at].definition.clone();
            let mut deaf = None;
            if !internal {
                for interface in &definition.interfaces {
                    let Some(port) = interface.port.resolve(&values) else {
                        continue;
                    };
                    if interface.transport != Transport::Tcp {
                        continue;
                    }
                    let listens = process::tcp_check(&self.peers, port)?;
                    facts.push(json!({"fact": "listens", "ok": listens, "port": port}));
                    if !listens && deaf.is_none() {
                        deaf = Some(port);
                    }
                }
            }
            healthy = self.check_health(at)? == Wellness::Healthy;
            let mut code = None;
            if let (false, Some(Health::Http { port, path })) = (internal, &definition.health)
                && let Some(port) = port.resolve(&values)
                && let Some((said, body)) =
                    process::http_said(&self.peers, port, &fill(path, &values))?
            {
                code = Some(said);
                missing = missing_in(&body);
            }
            if definition.health.is_some() {
                facts.push(
                    json!({"fact": "check", "ok": healthy, "code": code, "missing": missing}),
                );
            }
            if let (Some(port), Some(pid)) = (deaf, self.services[at].pid) {
                let ports = process::ports_of(&self.peers, pid)?;
                maybe = if ports.is_empty() {
                    json!({"kind": "not_listening", "port": port})
                } else {
                    json!({"kind": "elsewhere", "port": port, "ports": ports})
                };
            }
        } else if self.services[at].state == State::Failed {
            let instance = &self.services[at];
            let line = if internal {
                None
            } else {
                process::tail(&self.peers, &instance.id, 1)
                    .ok()
                    .and_then(|lines| lines.into_iter().next_back())
                    .map(|line| instance.hide(&self.own.secrets, line))
            };
            maybe = json!({"kind": "stopped_itself", "problem": instance.problem, "line": line});
        }

        // The others as last seen: asking each guest again is what a
        // service list does, and this is about one service.
        let down: Vec<usize> = (0..self.services.len())
            .filter(|&other| other != at && self.services[other].state != State::Running)
            .collect();
        for &other in &down {
            let instance = &self.services[other];
            facts.push(json!({
                "fact": "other", "ok": false, "service": instance.name,
                "state": instance.state.as_str(), "problem": instance.problem,
            }));
        }
        if self.peers.id("network").is_some() {
            facts.push(json!({"fact": "network", "ok": self.network_up()}));
        }

        if maybe.is_null() && running && !healthy {
            let after = &self.services[at].after;
            let by_word = missing.iter().find_map(|word| {
                let wanted = meant(word)?;
                down.iter()
                    .find(|&&other| self.services[other].definition.id == wanted)
                    .map(|&other| (other, Some(word.clone())))
            });
            let by_need = || {
                down.iter()
                    .find(|&&other| after.contains(&self.services[other].name))
                    .map(|&other| (other, None))
            };
            maybe = match by_word.or_else(by_need) {
                Some((other, word)) => json!({
                    "kind": "needs", "other": self.services[other].name,
                    "definition": self.services[other].definition.id, "word": word,
                }),
                None => json!({"kind": "check_failed"}),
            };
        }
        Ok(answer(facts, maybe))
    }
}
