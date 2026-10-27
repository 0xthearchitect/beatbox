//! Remembering what exists, across BeatBox closing and opening again.
//!
//! The engine keeps nothing on disk, so this does: one `state/<id>.json` per
//! sandbox, with what `SandboxRuntime::restore` needs and where it was.
//!
//! Written only on lifecycle changes: `sandbox.action` fires on every
//! command in a guest, and the record wouldn't change. Held to
//! `golden/api/records.json`, the same four keys the Python build wrote, so
//! upgrades and downgrades both find their sandboxes.

use std::fs;
use std::path::Path;
use std::sync::Weak;

use beatbox_engine::{
    EngineError, Event, Lifecycle, Reason, SandboxConfig, SandboxIdentity, SandboxRuntime,
};
use serde_json::{Value, json};

/// The events worth a write: exactly the ones a lifecycle change produces,
/// and the one that changes the configuration itself.
pub const LIFECYCLE_EVENTS: [&str; 10] = [
    "sandbox.created",
    "sandbox.started",
    "sandbox.stopped",
    "sandbox.paused",
    "sandbox.resumed",
    "sandbox.reset",
    "sandbox.failed",
    "sandbox.destroyed",
    "sandbox.reconfigured",
    "sandbox.renamed",
];

/// The subscriber: write the record down, or remove it on `destroyed`.
///
/// Reads the sandbox's handle, which never waits for its lane (the bus's
/// rule). The runtime is weak because it holds the bus that holds this.
pub(crate) fn persist(runtime: &Weak<SandboxRuntime>, state: &Path, event: &Event) {
    if !LIFECYCLE_EVENTS.contains(&event.name()) {
        return;
    }
    if event.name() == "sandbox.destroyed" {
        forget(state, event.sandbox_id());
        return;
    }
    let Some(runtime) = runtime.upgrade() else {
        return;
    };
    let Ok(sandbox) = runtime.get(event.sandbox_id()) else {
        return;
    };
    let record = json!({
        "identity": sandbox.identity().to_value(),
        "config": sandbox.config().to_value(),
        "lifecycle": sandbox.lifecycle().as_str(),
        "reason": sandbox.state().reason.as_ref().map(reason_to_value),
    });
    if let Err(error) = write(state, &sandbox.id(), &record) {
        log::error!("BeatBox could not write down {}: {error}", sandbox.id());
    }
}

/// Written to a neighbour and renamed in, so a kill mid-write leaves the
/// previous version, not half a file.
fn write(state: &Path, sandbox_id: &str, record: &Value) -> std::io::Result<()> {
    fs::create_dir_all(state)?;
    let target = state.join(format!("{sandbox_id}.json"));
    let partial = state.join(format!("{sandbox_id}.json.tmp"));
    let text = serde_json::to_string_pretty(record).expect("a record is JSON");
    fs::write(&partial, text)?;
    fs::rename(&partial, &target)
}

fn forget(state: &Path, sandbox_id: &str) {
    let _ = fs::remove_file(state.join(format!("{sandbox_id}.json")));
}

fn reason_to_value(reason: &Reason) -> Value {
    json!({"code": reason.code, "values": reason.values})
}

fn reason_from_value(raw: Option<&Value>) -> Option<Reason> {
    let raw = raw.filter(|raw| crate::py::truthy(raw))?;
    let code = raw
        .get("code")
        .map(crate::py::text_or_empty)
        .unwrap_or_default();
    let values = raw
        .get("values")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| (key.clone(), crate::py::text(value)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(Reason::new(code).with_values(values))
}

/// A record's text: UTF-8, or on Windows, Windows-1252, which the Python
/// build wrote there. Better than losing the sandbox.
fn decode(bytes: Vec<u8>) -> Option<String> {
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(error) if cfg!(windows) => Some(cp1252(error.as_bytes())),
        Err(_) => None,
    }
}

fn cp1252(bytes: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}',
        '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}',
        '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}',
        '\u{178}',
    ];
    bytes
        .iter()
        .map(|&byte| match byte {
            0x80..=0x9f => HIGH[usize::from(byte - 0x80)],
            _ => char::from(byte),
        })
        .collect()
}

/// Bring back every sandbox a previous run left known.
///
/// `restore`, never `create`: `create` would make each host sandbox a fresh
/// overlay, and every restored disk would start the session wiped.
///
/// Safe to call again, which [`crate::Api::register`] relies on: restored
/// records are skipped, and one for a type not registered yet waits.
///
/// Never takes the launch down: a record that isn't a JSON object is
/// removed, and one whose configuration can't be read is logged and kept.
pub(crate) fn restore_all(runtime: &SandboxRuntime, state: &Path) {
    let Ok(entries) = fs::read_dir(state) else {
        return;
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".json"))
        })
        .collect();
    paths.sort();
    let known: Vec<String> = runtime
        .every_sandbox()
        .iter()
        .map(beatbox_engine::Sandbox::id)
        .collect();

    for path in paths {
        let raw = fs::read(&path)
            .ok()
            .and_then(decode)
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let raw = match raw {
            Some(raw @ Value::Object(_)) => raw,
            _ => {
                log::error!(
                    "BeatBox could not read the sandbox recorded at {}",
                    path.display()
                );
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let identity = SandboxIdentity::from_value(raw.get("identity").unwrap_or(&Value::Null));
        if identity.id.is_empty() || identity.type_name.is_empty() {
            let _ = fs::remove_file(&path);
            continue;
        }
        if known.contains(&identity.id) {
            continue;
        }
        let config = match SandboxConfig::from_value(raw.get("config").unwrap_or(&Value::Null)) {
            Ok(config) => config,
            Err(error) => {
                log::error!(
                    "BeatBox could not read the configuration recorded at {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        let hint = raw
            .get("lifecycle")
            .map(crate::py::text_or_empty)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| Lifecycle::Created.as_str().to_owned());
        let Ok(hint) = hint.parse::<Lifecycle>() else {
            log::error!(
                "BeatBox could not restore the sandbox recorded at {}: no lifecycle {hint:?}",
                path.display()
            );
            continue;
        };
        match runtime.restore(identity, config, hint, reason_from_value(raw.get("reason"))) {
            Ok(_) | Err(EngineError::Refused(_)) => {}
            Err(error) => log::error!(
                "BeatBox could not restore the sandbox recorded at {}: {error}",
                path.display()
            ),
        }
    }
}
