//! A machine's kept UDP port that another program took while it was off:
//! starting it gives it another one, at the same address. Only on the UDP
//! wire (Windows, or `BEATBOX_NETWORK_WIRE=udp`): a file never clashes.

mod support;

use std::net::UdpSocket;

use beatbox_api::{Api, ApiParts};
use serde_json::{Value, json};
use support::{Room, data, parts};

fn cable(api: &Api, network: &str) -> Value {
    let answer = data(api.dispatch(
        "sandbox.act",
        &json!({"id": network, "capability": "network.attach", "arguments": {"node": "vm"}}),
    ));
    answer["data"].clone()
}

fn settings_of(api: &Api, id: &str) -> Value {
    data(api.dispatch("sandbox.refresh", &json!({"id": id})))["config"]["settings"].clone()
}

fn transition(api: &Api, id: &str, transition: &str) {
    data(api.dispatch(
        "sandbox.transition",
        &json!({"id": id, "transition": transition}),
    ));
}

#[test]
fn a_machine_whose_port_was_taken_while_off_starts_on_another() {
    let udp = cfg!(windows) || std::env::var("BEATBOX_NETWORK_WIRE").as_deref() == Ok("udp");
    if !udp {
        eprintln!("not on the UDP wire: skipped");
        return;
    }
    let room = Room::new("recable");
    let mut parts: ApiParts = parts(room.path());
    parts.types.push(beatbox_network::NetworkSandbox::spec());
    let api = Api::open(parts);
    let made = |kind: &str, settings: Value| {
        let made = data(api.dispatch(
            "sandbox.create",
            &json!({"type": kind, "name": kind, "cpu": 0, "memory": 0, "settings": settings, "uses": {}}),
        ));
        made["id"].as_str().unwrap().to_owned()
    };
    let network = made("network", json!({}));
    transition(&api, &network, "start");
    let mut first = cable(&api, &network);
    first["sandbox"] = json!(network);
    let machine = made("mock", json!({"network": first}));

    let kept = first["port"]
        .as_str()
        .unwrap()
        .strip_prefix("udp:")
        .unwrap();
    let squatter = UdpSocket::bind(kept).unwrap();
    transition(&api, &machine, "start");
    let now = settings_of(&api, &machine)["network"].clone();
    assert_ne!(now["port"], first["port"]);
    assert_eq!(now["address"], first["address"]);
    assert_eq!(now["sandbox"], json!(network));
    assert_eq!(cable(&api, &network)["port"], now["port"]);

    // A free port stays as it is.
    drop(squatter);
    transition(&api, &machine, "stop");
    transition(&api, &machine, "start");
    assert_eq!(settings_of(&api, &machine)["network"]["port"], now["port"]);
}
