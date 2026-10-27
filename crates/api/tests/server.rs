//! A Server through the daemon's door: made with `uses`, shown by id, given
//! its library, and the same after the daemon comes back.
//!
//! The Host is the mock, which declares everything the `host` role asks for.

mod support;

use beatbox_api::Api;
use beatbox_engine::MockSandbox;
use beatbox_server::ServerSandbox;
use serde_json::{Value, json};
use support::{Room, call, data, reason_code};

fn open(room: &Room) -> Api {
    let mut parts = support::parts(room.path());
    let mut host = MockSandbox::spec();
    host.capabilities.push("network.interfaces".into());
    parts.types = vec![host, ServerSandbox::spec()];
    Api::open(parts)
}

fn create(api: &Api, kind: &str, name: &str, uses: Value) -> beatbox_contract::Envelope {
    call(
        api,
        "sandbox.create",
        json!({"type": kind, "name": name, "cpu": 0, "memory": 0, "settings": {}, "uses": uses}),
    )
}

fn act(api: &Api, id: &str, capability: &str, arguments: Value) -> Value {
    let outcome = data(call(
        api,
        "sandbox.act",
        json!({"id": id, "capability": capability, "arguments": arguments}),
    ));
    assert_eq!(outcome["ok"], true, "{outcome}");
    outcome["data"].clone()
}

#[test]
fn a_server_is_made_through_the_door_using_a_host_by_name() {
    let room = Room::new("server");
    let api = open(&room);
    let host = data(create(&api, "mock", "box", json!({})));
    let host_id = host["id"].as_str().unwrap().to_owned();
    assert_eq!(host["config"]["uses"], json!({}));

    let server = data(create(&api, "server", "srv", json!({"host": "box"})));
    // Shown by id: renaming the Host does not lose it.
    assert_eq!(server["config"]["uses"], json!({"host": host_id}));
    // Its library is the daemon's, unless the caller said otherwise.
    assert_eq!(
        server["config"]["settings"]["definitions"],
        room.path().join("services").to_string_lossy().as_ref()
    );
}

#[test]
fn the_type_and_what_each_of_its_actions_takes_are_published() {
    let room = Room::new("server");
    let api = open(&room);
    let types = data(call(&api, "sandbox.types", json!({})));
    let server = types
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["name"] == "server")
        .expect("the server type is registered");
    let actions: Vec<&str> = server["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|spec| spec["capability"].as_str().unwrap())
        .collect();
    for capability in [
        "service.create",
        "service.start",
        "service.call",
        "service.describe",
    ] {
        assert!(actions.contains(&capability), "{capability}: {actions:?}");
    }
}

#[test]
fn a_definition_dropped_into_the_library_is_in_the_catalog() {
    let room = Room::new("server");
    let api = open(&room);
    let library = room.path().join("services");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(
        library.join("erp.json"),
        json!({
            "id": "erp", "version": "1", "name": "ERP",
            "runtime": {"type": "process", "command": ["/srv/erp"]}
        })
        .to_string(),
    )
    .unwrap();
    data(create(&api, "mock", "box", json!({})));
    let server = data(create(&api, "server", "srv", json!({"host": "box"})));
    let id = server["id"].as_str().unwrap();
    let catalog = act(&api, id, "service.catalog", json!({}));
    let erp = catalog["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "erp")
        .expect("the library's definition");
    assert_eq!(erp["source"], "library");
}

#[test]
fn a_peer_that_is_not_there_is_refused_by_name() {
    let room = Room::new("server");
    let api = open(&room);
    let refused = create(&api, "server", "srv", json!({"host": "nowhere"}));
    assert_eq!(reason_code(&refused), "engine.peer_unknown");
    let refused = create(&api, "server", "srv", json!({"disk": "nowhere"}));
    assert_eq!(reason_code(&refused), "engine.peer_unexpected");
}

#[test]
fn who_it_uses_survives_the_daemon_going_away() {
    let room = Room::new("server");
    let (host_id, server_id) = {
        let api = open(&room);
        let host = data(create(&api, "mock", "box", json!({})));
        let server = data(create(&api, "server", "srv", json!({"host": "box"})));
        let id = server["id"].as_str().unwrap().to_owned();
        act(
            &api,
            &id,
            "service.create",
            json!({"definition": "http-static", "name": "site"}),
        );
        (host["id"].as_str().unwrap().to_owned(), id)
    };
    let api = open(&room);
    let server = data(call(&api, "sandbox.view", json!({"id": server_id})));
    assert_eq!(server["config"]["uses"], json!({"host": host_id}));
    let listed = act(&api, &server_id, "service.list", json!({}));
    assert_eq!(listed["services"][0]["service"], "site");
}

#[test]
fn a_server_that_is_off_is_pointed_at_another_machine() {
    let room = Room::new("server");
    let api = open(&room);
    let first = data(create(&api, "mock", "box", json!({})));
    let second = data(create(&api, "mock", "box-2", json!({})));
    let server = data(create(&api, "server", "srv", json!({"host": "box"})));
    let id = server["id"].as_str().unwrap().to_owned();

    let moved = data(call(
        &api,
        "sandbox.use",
        json!({"id": id, "uses": {"host": "box-2"}}),
    ));
    assert_eq!(moved["config"]["uses"], json!({"host": second["id"]}));
    assert_ne!(moved["config"]["uses"]["host"], first["id"]);

    // Said in words, and nothing changes, when the machine is not there.
    let refused = call(
        &api,
        "sandbox.use",
        json!({"id": id, "uses": {"host": "nowhere"}}),
    );
    assert_eq!(reason_code(&refused), "engine.peer_unknown");
    let still = data(call(&api, "sandbox.view", json!({"id": id})));
    assert_eq!(still["config"]["uses"], json!({"host": second["id"]}));

    // Not while it is running.
    data(call(
        &api,
        "sandbox.transition",
        json!({"id": id, "transition": "start"}),
    ));
    let refused = call(
        &api,
        "sandbox.use",
        json!({"id": id, "uses": {"host": "box"}}),
    );
    assert_eq!(reason_code(&refused), "engine.reconfigure_not_now");
}
