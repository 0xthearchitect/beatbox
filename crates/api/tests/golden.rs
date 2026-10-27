//! The facade against the recorded one.
//!
//! `golden/api/walk.json`: 33 requests against `MockSandbox` with a fixed
//! clock and ids, and their envelopes. `golden/api/records.json`: the record
//! after each lifecycle change. Both must come back the same, and every
//! refusal in `golden/contract/envelopes.json` must be refused the same way.

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use beatbox_api::records::LIFECYCLE_EVENTS;
use beatbox_api::{Api, ApiParts};
use beatbox_engine::{MockSandbox, RuntimeParts};
use serde_json::{Map, Value, json};
use support::{Room, parts};

const DIR: &str = "{DIR}";

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../golden")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// Equal as JSON, and as numbers: `4` and `4.0` are one value.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// Every string with `from` in it, with `to` in its place.
fn swap(value: &Value, from: &str, to: &str) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace(from, to)),
        Value::Array(items) => Value::Array(items.iter().map(|v| swap(v, from, to)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.replace(from, to), swap(v, from, to)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A path with `/` separators, as the goldens were recorded on Linux.
fn as_recorded(value: Value) -> Value {
    if cfg!(windows) {
        swap(&value, "\\", "/")
    } else {
        value
    }
}

/// `2026-09-24T12:00:00+00:00`, and one second later every time it is asked.
fn monotonic_clock() -> RuntimeParts {
    let count = Arc::new(Mutex::new(0u32));
    let ids = Arc::new(Mutex::new(0u32));
    RuntimeParts {
        clock: Some(Arc::new(move || {
            let mut n = count.lock().unwrap();
            let now = format!("2026-09-24T12:{:02}:{:02}+00:00", *n / 60, *n % 60);
            *n += 1;
            now
        })),
        ids: Some(Arc::new(move || {
            let mut n = ids.lock().unwrap();
            *n += 1;
            format!("sbx-{:012}", *n)
        })),
        ..RuntimeParts::default()
    }
}

struct Walk {
    _room: Room,
    tree: String,
    api: Api,
    records: Arc<Mutex<Map<String, Value>>>,
}

fn walk() -> Walk {
    let room = Room::new("golden");
    let tree = room.path().to_string_lossy().into_owned();
    let home = room.join("beatbox-home");
    let rooms = Arc::new(Mutex::new(0u32));
    let machines = home.join("machines");
    let mut parts = parts(room.path());
    parts.paths.state = home.join("state");
    parts.paths.machines = machines.clone();
    let api = Api::open(ApiParts {
        types: vec![MockSandbox::spec()],
        runtime: monotonic_clock(),
        workspace: Some(Arc::new(move || {
            let mut n = rooms.lock().unwrap();
            *n += 1;
            machines
                .join(format!("sbx-room{:04}", *n))
                .to_string_lossy()
                .into_owned()
        })),
        ..parts
    });

    // After the facade's own subscriber, so it reads what was just written.
    let records = Arc::new(Mutex::new(Map::new()));
    let kept = Arc::clone(&records);
    let state = home.join("state");
    let root = tree.clone();
    api.runtime()
        .events()
        .subscribe("sandbox.*", move |event| {
            if !LIFECYCLE_EVENTS.contains(&event.name()) {
                return;
            }
            let path = state.join(format!("{}.json", event.sandbox_id()));
            let record = std::fs::read_to_string(&path)
                .ok()
                .map(|text| as_recorded(swap(&serde_json::from_str(&text).unwrap(), &root, DIR)));
            kept.lock()
                .unwrap()
                .entry(event.sandbox_id().to_owned())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap()
                .push(json!({"after": event.name(), "record": record}));
        })
        .unwrap();
    Walk {
        _room: room,
        tree,
        api,
        records,
    }
}

#[test]
fn the_walk_answers_every_request_as_the_python_did() {
    let recorded = golden("api/walk.json");
    let walk = walk();
    for (index, step) in recorded["steps"].as_array().unwrap().iter().enumerate() {
        let action = step["action"].as_str().unwrap();
        let note = step.get("note").and_then(Value::as_str).unwrap_or_default();
        if note == "crashed, not yet looked at" {
            let doomed = step["payload"]["id"].as_str().unwrap();
            walk.api
                .runtime()
                .get(doomed)
                .unwrap()
                .provider::<MockSandbox, _>(MockSandbox::crash)
                .unwrap();
        }
        let payload = swap(&step["payload"], DIR, &walk.tree);
        let envelope = walk.api.dispatch(action, &payload);
        let sent = as_recorded(swap(
            &serde_json::to_value(&envelope).unwrap(),
            &walk.tree,
            DIR,
        ));
        assert!(
            same(&sent, &step["envelope"]),
            "step {index} ({action}{}):\n sent:     {sent}\n recorded: {}",
            if note.is_empty() {
                String::new()
            } else {
                format!(", {note}")
            },
            step["envelope"]
        );
    }
}

#[test]
fn the_walk_leaves_the_records_the_python_left() {
    let recorded = golden("api/records.json");
    let steps = golden("api/walk.json");
    let walk = walk();
    for step in steps["steps"].as_array().unwrap() {
        if step.get("note").and_then(Value::as_str) == Some("crashed, not yet looked at") {
            walk.api
                .runtime()
                .get(step["payload"]["id"].as_str().unwrap())
                .unwrap()
                .provider::<MockSandbox, _>(MockSandbox::crash);
        }
        walk.api.dispatch(
            step["action"].as_str().unwrap(),
            &swap(&step["payload"], DIR, &walk.tree),
        );
    }
    let kept = Value::Object(walk.records.lock().unwrap().clone());
    let expected = &recorded["records"];
    for (sandbox_id, history) in expected.as_object().unwrap() {
        let ours = &kept[sandbox_id];
        let theirs = history.as_array().unwrap();
        assert_eq!(
            ours.as_array().map(Vec::len),
            Some(theirs.len()),
            "{sandbox_id}: {ours}"
        );
        for (index, (ours, theirs)) in ours.as_array().unwrap().iter().zip(theirs).enumerate() {
            assert!(
                same(ours, theirs),
                "{sandbox_id}, record {index}:\n ours:   {ours}\n theirs: {theirs}"
            );
        }
    }
    assert_eq!(
        kept.as_object().unwrap().len(),
        expected.as_object().unwrap().len()
    );
}

#[test]
fn every_refusal_at_the_door_is_the_pythons() {
    let refusals = golden("contract/envelopes.json");
    let room = Room::new("golden");
    let api = support::open(room.path());
    let check = |action: &str, payload: Value, recorded: &Value| {
        let sent = serde_json::to_value(api.dispatch(action, &payload)).unwrap();
        assert_eq!(&sent, recorded, "{action} {payload}");
    };
    check("no.such", json!({}), &refusals["unknown-action"]);
    for spec in beatbox_contract::ACTIONS {
        // The golden was recorded before BeatDrop existed. New actions have
        // contract tests of their own; this file compares only recorded ones.
        if refusals["unexpected"].get(spec.name).is_none() {
            continue;
        }
        check(
            spec.name,
            json!({"bogusField": 1, "zeta": 2}),
            &refusals["unexpected"][spec.name],
        );
        if !spec.payload.is_empty() {
            check(spec.name, json!({}), &refusals["missing"][spec.name]);
        }
    }
}
