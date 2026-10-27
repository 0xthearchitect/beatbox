//! Round-trip the recorded goldens through the generated types:
//! `golden/contract/types.json` (every type) and `golden/api/walk.json`
//! (33 real answers). Read strictly, write back, must come out identical.

use std::path::PathBuf;

use beatbox_contract::{ACTIONS, EVENTS, TYPES};
use serde_json::Value;

#[allow(missing_docs)]
mod generated {
    use beatbox_contract::*;
    include!(concat!(env!("OUT_DIR"), "/reserialize.rs"));
}

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../golden")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{name} is not JSON: {error}"))
}

/// JSON-equal, with numbers compared as numbers (`2.0` == `2`, the goldens
/// have Python floats). Everything else exact, keys included.
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

/// Read `value` as `kind`, in the contract's type language, and write it back.
fn round_trip(kind: &str, value: &Value, context: &str) {
    if let Some(inner) = kind.strip_suffix('?') {
        if !value.is_null() {
            round_trip(inner, value, context);
        }
        return;
    }
    if let Some(item) = kind.strip_suffix("[]") {
        let items = value
            .as_array()
            .unwrap_or_else(|| panic!("{context}: {kind} is not a list"));
        for (index, value) in items.iter().enumerate() {
            round_trip(item, value, &format!("{context}[{index}]"));
        }
        return;
    }
    match kind {
        "str" => assert!(value.is_string(), "{context}: not a str"),
        "int" => assert!(value.is_i64() || value.is_u64(), "{context}: not an int"),
        "float" => assert!(value.is_number(), "{context}: not a float"),
        "bool" => assert!(value.is_boolean(), "{context}: not a bool"),
        "any" => {}
        name => {
            let back = generated::reserialize(name, value.clone())
                .unwrap_or_else(|| panic!("{context}: {name} is not a contract type"))
                .unwrap_or_else(|error| panic!("{context}: not a {name}: {error}"));
            assert!(
                same(value, &back),
                "{context}: {name} did not come back the same\n sent: {value}\n back: {back}"
            );
        }
    }
}

#[test]
fn every_type_comes_back_as_the_python_sent_it() {
    let recorded = golden("contract/types.json");
    let recorded = recorded.as_object().expect("types.json is an object");
    let names: Vec<&str> = recorded.keys().map(String::as_str).collect();
    assert!(
        names.iter().all(|name| TYPES.contains(name)),
        "a type recorded before BeatDrop disappeared from contract.json"
    );
    for (name, shapes) in recorded {
        for variant in ["full", "sparse"] {
            round_trip(name, &shapes[variant], &format!("{name} ({variant})"));
        }
    }
}

#[test]
fn every_answer_in_the_facade_walk_has_the_shape_its_action_declares() {
    let walk = golden("api/walk.json");
    let steps = walk["steps"].as_array().expect("walk.json has steps");
    assert!(
        steps.len() >= 30,
        "the walk is shorter than it was recorded"
    );
    for (index, step) in steps.iter().enumerate() {
        let name = step["action"].as_str().expect("a step names its action");
        let context = format!("step {index} ({name})");
        let envelope = &step["envelope"];
        round_trip("Envelope", envelope, &context);
        if envelope["ok"] == Value::Bool(true) {
            let spec = beatbox_contract::action(name)
                .unwrap_or_else(|| panic!("{context}: not in contract.json"));
            round_trip(spec.response, &envelope["data"], &context);
        } else {
            round_trip("ApiError", &envelope["error"], &context);
        }
    }
}

#[test]
fn every_refusal_at_the_door_is_an_envelope() {
    let refusals = golden("contract/envelopes.json");
    let mut seen = 0;
    let mut walk = |value: &Value, context: &str| {
        round_trip("Envelope", value, context);
        assert_eq!(
            value["ok"],
            Value::Bool(false),
            "{context}: a refusal said ok"
        );
        seen += 1;
    };
    for (key, value) in refusals.as_object().expect("envelopes.json is an object") {
        if value.get("ok").is_some() {
            walk(value, key);
        } else {
            for (action, envelope) in value.as_object().expect("a group of envelopes") {
                walk(envelope, &format!("{key} {action}"));
            }
        }
    }
    // Unknown action, an extra field per action, missing payloads, and the two closing answers.
    assert_eq!(seen, 58, "the recorded refusals changed");
}

#[test]
fn the_contract_is_what_the_python_answers() {
    // Bump when a recorded shape changes. Adding actions alone doesn't need it.
    assert_eq!(beatbox_contract::CONTRACT_VERSION, 5);
    assert_eq!(
        ACTIONS.len(),
        48,
        "contract.json has a different number of actions"
    );
    assert_eq!(
        EVENTS.len(),
        4,
        "contract.json has a different number of events"
    );
    for spec in ACTIONS {
        let segments: Vec<&str> = spec.name.split('.').collect();
        assert!(
            segments.len() >= 2 && segments.iter().all(|s| !s.is_empty()),
            "{} is not namespace.verb",
            spec.name
        );
    }
}

/// Files, language and window are the shell's; machines are the daemon's.
#[test]
fn each_half_answers_what_belongs_to_it() {
    use beatbox_contract::AnsweredBy;
    let shell: Vec<&str> = ACTIONS
        .iter()
        .filter(|spec| spec.answered_by == AnsweredBy::Shell)
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        shell,
        [
            "files.list",
            "files.places",
            "files.read",
            "host.closing",
            "host.display",
            "host.reveal",
            "system.about",
            "system.locale",
            "system.preferences",
            "system.set_language",
            "system.set_preferences"
        ]
    );
    let shell_events: Vec<&str> = EVENTS
        .iter()
        .filter(|spec| spec.answered_by == AnsweredBy::Shell)
        .map(|spec| spec.name)
        .collect();
    assert_eq!(shell_events, ["closing", "locale"]);
}

/// The door's refusals match the recorded ones word for word, for every
/// action. The two closing answers come from the shell's handler, not the door.
#[test]
fn the_door_refuses_what_the_pythons_refused() {
    use beatbox_contract::door;
    let refusals = golden("contract/envelopes.json");
    let check = |action: &str, payload: Value, recorded: &Value| {
        let error = door::admit(action, &payload)
            .err()
            .unwrap_or_else(|| panic!("{action} {payload} was let in"));
        let sent = serde_json::to_value(door::refused(error)).unwrap();
        assert_eq!(&sent, recorded, "{action} {payload}");
    };
    check(
        "no.such",
        serde_json::json!({}),
        &refusals["unknown-action"],
    );
    for spec in ACTIONS {
        if refusals["unexpected"].get(spec.name).is_none() {
            continue;
        }
        check(
            spec.name,
            serde_json::json!({"bogusField": 1, "zeta": 2}),
            &refusals["unexpected"][spec.name],
        );
        if !spec.payload.is_empty() {
            check(
                spec.name,
                serde_json::json!({}),
                &refusals["missing"][spec.name],
            );
        }
    }
}

#[test]
fn the_door_lets_in_the_pages_spelling_and_hands_on_the_facades() {
    use beatbox_contract::door;
    let (spec, payload) = door::admit(
        "disks.build",
        &serde_json::json!({"source": "/d.iso", "name": "w", "sizeGb": 20, "network": false, "unattended": false, "machine": null}),
    )
    .unwrap();
    assert_eq!(spec.name, "disks.build");
    assert_eq!(payload["size_gb"], 20);
    // Top level only; nested keys belong to the provider.
    let (_, payload) = door::admit(
        "sandbox.act",
        &serde_json::json!({"id": "x", "capability": "a.b", "arguments": {"someKey": 1}}),
    )
    .unwrap();
    assert_eq!(payload["arguments"], serde_json::json!({"someKey": 1}));
    assert!(door::admit("sandbox.list", &Value::Null).is_ok());
    assert_eq!(
        door::admit("sandbox.list", &serde_json::json!([1]))
            .unwrap_err()
            .code,
        "bad-payload"
    );
}
