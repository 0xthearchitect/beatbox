//! The contract is only worth having if something checks it. The page's half
//! is `web/tests/contract.test.mjs`.

mod support;

use std::collections::BTreeSet;

use beatbox_api::HANDLED;
use beatbox_contract::{ACTIONS, AnsweredBy};
use serde_json::{Value, json};
use support::{Room, call, code, open};

/// An action in the contract with no handler is a button that answers
/// "unknown-action"; a handler with no entry is invisible to the page.
#[test]
fn test_every_contract_action_has_a_handler() {
    let daemon: BTreeSet<&str> = ACTIONS
        .iter()
        .filter(|spec| spec.answered_by == AnsweredBy::Daemon)
        .map(|spec| spec.name)
        .collect();
    let handled: BTreeSet<&str> = HANDLED.into_iter().collect();
    assert_eq!(handled, daemon);
}

#[test]
fn test_unknown_action_is_an_envelope_and_not_an_exception() {
    let room = Room::new("contract");
    let api = open(room.path());
    let envelope = call(&api, "no.such.action", json!({}));
    assert!(!envelope.ok);
    assert_eq!(code(&envelope), "unknown-action");
    let sent = serde_json::to_value(&envelope).unwrap();
    let keys: BTreeSet<&str> = sent
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, BTreeSet::from(["data", "error", "ok"]));
}

#[test]
fn test_unexpected_payload_field_is_refused() {
    let room = Room::new("contract");
    let api = open(room.path());
    assert_eq!(
        code(&call(&api, "system.probe", json!({"surprise": 1}))),
        "bad-payload"
    );
}

/// What the shell answers is in the contract, so it is not unknown here --
/// it is the other half's, and the daemon says so.
#[test]
fn test_what_the_shell_answers_is_refused_by_name() {
    let room = Room::new("contract");
    let api = open(room.path());
    for spec in ACTIONS
        .iter()
        .filter(|spec| spec.answered_by == AnsweredBy::Shell)
    {
        let payload: serde_json::Map<String, Value> = spec
            .payload
            .iter()
            .map(|(field, _)| (beatbox_contract::door::camel(field), json!("")))
            .collect();
        assert_eq!(
            code(&call(&api, spec.name, Value::Object(payload))),
            "not-here",
            "{}",
            spec.name
        );
    }
}

/// It crosses a socket as text. Read-only actions only: dispatching every
/// action once left containers and a network on a developer's machine.
#[test]
fn test_envelope_is_json_serialisable() {
    let room = Room::new("contract");
    let api = open(room.path());
    let sample = |field: &str| match field {
        "id" | "sandbox_id" => json!("sbx-nobody"),
        "since" => json!(0),
        "path" => json!("/no/such/disk.vdi"),
        other => panic!("no sample for {other}"),
    };
    for spec in ACTIONS
        .iter()
        .filter(|spec| !spec.mutates && spec.answered_by == AnsweredBy::Daemon)
    {
        let payload: serde_json::Map<String, Value> = spec
            .payload
            .iter()
            .map(|(field, _)| (beatbox_contract::door::camel(field), sample(field)))
            .collect();
        let envelope = call(&api, spec.name, Value::Object(payload));
        serde_json::to_string(&envelope).unwrap();
    }
}

/// Adding an action forces a decision about whether it is safe to call.
#[test]
fn test_every_world_changing_action_is_declared_as_one() {
    let declared: BTreeSet<&str> = ACTIONS
        .iter()
        .filter(|spec| spec.mutates)
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        declared,
        BTreeSet::from([
            "system.set_language",
            "system.set_preferences",
            "storage.sweep",
            "sandbox.create",
            "sandbox.transition",
            "sandbox.act",
            "sandbox.rename",
            "sandbox.reconfigure",
            "sandbox.use",
            "disks.build",
            "beatdrop.start",
            "beatdrop.cancel",
            "disks.finish",
            "disks.forget",
            "tools.fetch",
            "tools.insert",
            "setup.fix",
            "disks.unpack",
            "host.closing",
            "host.display",
            "commands.save",
            "commands.forget",
            "watchers.save",
            "watchers.forget",
        ]),
        "a new action needs `mutates` set, or explicitly left out after thinking about it"
    );
}

/// A type says what its actions take and give back, so a page can draw a
/// form for one.
#[test]
fn test_sandbox_types_say_what_each_action_takes() {
    let room = Room::new("contract");
    let mut parts = support::parts(room.path());
    parts.types.push(beatbox_network::NetworkSandbox::spec());
    let api = beatbox_api::Api::open(parts);
    let types = support::data(call(&api, "sandbox.types", json!({})));
    let network = types
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["name"] == "network")
        .expect("the network type is registered");
    let add = network["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|spec| spec["capability"] == "firewall.add")
        .expect("firewall.add has a spec");
    assert_eq!(
        add["inputs"][0],
        json!({"name": "action", "kind": "text", "required": true})
    );
    assert_eq!(
        add["outputs"],
        json!([{"name": "id", "kind": "text", "required": true}])
    );
}
