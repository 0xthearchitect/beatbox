//! Commands kept for running inside machines, and the file they are kept in.
//!
//! A line of shell with blanks (`ping -c 4 {node}`), one per OS, run through
//! `process.shell`. An empty `machine` offers it everywhere. The built-ins
//! live in the page; this file holds only what somebody wrote.

use std::fs;
use std::path::Path;

use beatbox_contract::CommandView;
use serde_json::{Map, Value};

use crate::fault::ApiFault;
use crate::{Api, py};

/// Where the kept commands are, under BeatBox's root.
pub const FILE: &str = "commands.json";

/// How long a command may take unless it says, and at most.
pub const TIMEOUT: i64 = 30;
/// See [`TIMEOUT`]; the host sandbox's own ceiling.
pub const TIMEOUT_MAX: i64 = 300;

/// Everything in `path`, or nothing: a file that cannot be read is an empty
/// library, not a daemon that will not start.
pub(crate) fn read_list<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Written to a neighbour and renamed in, as the records are.
pub(crate) fn write_list<T: serde::Serialize + ?Sized>(
    path: &Path,
    items: &T,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let next = path.with_extension("json.next");
    fs::write(
        &next,
        serde_json::to_vec_pretty(items).expect("a list is JSON"),
    )?;
    fs::rename(next, path)
}

/// A short id nobody has to read.
pub(crate) fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        &beatbox_isolation::uuid4().replace('-', "")[..10]
    )
}

fn text(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .map(py::text_or_empty)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn bad(message: &str) -> ApiFault {
    ApiFault::new("bad-payload", message)
}

impl Api {
    fn commands_path(&self) -> std::path::PathBuf {
        self.paths().root.join(FILE)
    }

    pub(crate) fn commands_list(&self) -> Result<Value, ApiFault> {
        let list: Vec<CommandView> = read_list(&self.commands_path());
        Ok(serde_json::to_value(list).expect("JSON"))
    }

    pub(crate) fn commands_save(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let Some(Value::Object(raw)) = payload.get("command") else {
            return Err(bad("A command is an object."));
        };
        let name = text(raw, "name");
        let (linux, windows) = (text(raw, "linux"), text(raw, "windows"));
        if name.is_empty() {
            return Err(bad("A command needs a name."));
        }
        if linux.is_empty() && windows.is_empty() {
            return Err(bad("A command needs a line for Linux, Windows or both."));
        }
        let blanks = match raw.get("blanks") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(Value::Object(blanks)) => Value::Object(blanks.clone()),
            Some(_) => return Err(bad("A command's blanks are an object.")),
        };
        let timeout = raw
            .get("timeout")
            .and_then(Value::as_i64)
            .unwrap_or(TIMEOUT)
            .clamp(1, TIMEOUT_MAX);
        let _kept = self.keeping();
        let mut list: Vec<CommandView> = read_list(&self.commands_path());
        let id = Some(text(raw, "id")).filter(|id| !id.is_empty());
        let command = CommandView {
            id: id.clone().unwrap_or_else(|| new_id("cmd")),
            name,
            group: Some(text(raw, "group"))
                .filter(|g| !g.is_empty())
                .unwrap_or_else(|| "Yours".into()),
            machine: text(raw, "machine"),
            linux,
            windows,
            blanks,
            timeout,
        };
        match list.iter_mut().find(|kept| Some(&kept.id) == id.as_ref()) {
            Some(kept) => *kept = command,
            None => list.push(command),
        }
        write_list(&self.commands_path(), &list).map_err(|error| {
            ApiFault::new("internal", format!("could not keep the command: {error}"))
        })?;
        Ok(serde_json::to_value(list).expect("JSON"))
    }

    pub(crate) fn commands_forget(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let id = payload.get("id").map(py::text_or_empty).unwrap_or_default();
        let _kept = self.keeping();
        let mut list: Vec<CommandView> = read_list(&self.commands_path());
        list.retain(|command| command.id != id);
        write_list(&self.commands_path(), &list).map_err(|error| {
            ApiFault::new("internal", format!("could not forget the command: {error}"))
        })?;
        Ok(serde_json::to_value(list).expect("JSON"))
    }
}
