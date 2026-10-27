//! The engine's walk against `golden/engine/walk.json` and
//! `walk-crash.json`, recorded from the Python engine. Same clock, same ids:
//! every event, outcome and state has to match, sequence and moment included.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{
    MockSandbox, Resources, RuntimeParts, SandboxConfig, SandboxRegistry, SandboxRuntime, data,
};
use serde_json::{Value, json};

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../golden")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// Equal as JSON, numbers compared as numbers: Python writes `4.0` for a
/// float and `4` for an int, and what matters is that they are four.
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

/// The recorder's clock: 12:00:00, then a second later every time.
fn monotonic_clock() -> beatbox_engine::clock::Clock {
    let count = Arc::new(Mutex::new(0u64));
    Arc::new(move || {
        let mut count = count.lock().unwrap();
        let n = *count;
        *count += 1;
        format!("2026-09-24T12:{:02}:{:02}+00:00", n / 60, n % 60)
    })
}

/// The recorder's ids: `sbx-000000000001`, and on.
fn ids() -> beatbox_engine::clock::Ids {
    let count = Arc::new(Mutex::new(0u64));
    Arc::new(move || {
        let mut count = count.lock().unwrap();
        *count += 1;
        format!("sbx-{:012}", *count)
    })
}

fn walk(crash: bool) -> Value {
    let mut registry = SandboxRegistry::default();
    registry.register(MockSandbox::spec()).unwrap();
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        clock: Some(monotonic_clock()),
        ids: Some(ids()),
        ..RuntimeParts::default()
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::clone(&events);
    runtime
        .events()
        .subscribe("*", move |event| {
            heard.lock().unwrap().push(event.to_value())
        })
        .unwrap();

    let mut out = serde_json::Map::new();
    out.insert(
        "types".into(),
        runtime
            .registry()
            .types()
            .iter()
            .map(|t| t.to_value())
            .collect(),
    );
    out.insert(
        "providing_process_execute".into(),
        json!(
            runtime
                .registry()
                .providing("process.execute")
                .iter()
                .map(|t| t.name.clone())
                .collect::<Vec<_>>()
        ),
    );
    let config = SandboxConfig::new("mock")
        .unwrap()
        .named("windows-client")
        .with_resources(Resources::new([(CPU, 4.0), (MEMORY, 8192.0)]).unwrap())
        .with_settings(data!({"files": {"/etc/hosts": "127.0.0.1 localhost"}}));
    let sandbox = runtime.create(config.clone()).unwrap();
    out.insert(
        "created".into(),
        json!({
            "identity": sandbox.identity().to_value(),
            "config": config.to_value(),
            "state": sandbox.state().to_value(),
        }),
    );
    sandbox.start().unwrap();
    out.insert("started".into(), sandbox.state().to_value());

    let steps = [
        ("process.execute", data!({"command": "/tmp/sample.exe"})),
        (
            "filesystem.write",
            data!({"path": "/tmp/note", "content": "it wrote something"}),
        ),
        ("snapshot.create", data!({"name": "before_attack"})),
        ("filesystem.read", data!({"path": "/tmp/note"})),
        ("filesystem.read", data!({"path": "/tmp/nowhere"})),
        ("traffic.capture", data!({})),
    ];
    let outcomes: Vec<Value> = steps
        .into_iter()
        .map(|(capability, arguments)| sandbox.act(capability, arguments).unwrap().to_value())
        .collect();
    out.insert("outcomes".into(), Value::Array(outcomes));

    if crash {
        sandbox.provider(|p: &mut MockSandbox| p.crash()).unwrap();
        out.insert("before_asking".into(), json!(sandbox.lifecycle().as_str()));
        sandbox.refresh().unwrap();
        out.insert("after_asking".into(), sandbox.state().to_value());
        sandbox.reset().unwrap();
        out.insert("reset".into(), sandbox.state().to_value());
    } else {
        sandbox.pause().unwrap();
        sandbox.resume().unwrap();
        sandbox.stop().unwrap();
        out.insert("stopped".into(), sandbox.state().to_value());
    }
    sandbox.destroy().unwrap();
    out.insert("destroyed".into(), sandbox.state().to_value());
    out.insert(
        "ledger_after".into(),
        Value::Object(runtime.resources().committed().to_data()),
    );
    out.insert("published".into(), json!(runtime.events().published()));
    out.insert(
        "events".into(),
        Value::Array(events.lock().unwrap().clone()),
    );
    Value::Object(out)
}

/// Where two walks first part, said precisely enough to fix.
fn assert_same_walk(ours: &Value, theirs: &Value) {
    let (ours, theirs) = (ours.as_object().unwrap(), theirs.as_object().unwrap());
    let mut keys: Vec<&String> = theirs.keys().collect();
    keys.sort();
    let mut mine: Vec<&String> = ours.keys().collect();
    mine.sort();
    assert_eq!(mine, keys, "the walks record different things");
    for key in keys {
        if key == "events" {
            let (a, b) = (
                ours[key].as_array().unwrap(),
                theirs[key].as_array().unwrap(),
            );
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                assert!(
                    same(x, y),
                    "event {index} differs\n rust:   {x}\n python: {y}"
                );
            }
            assert_eq!(a.len(), b.len(), "a different number of events");
        } else {
            assert!(
                same(&ours[key], &theirs[key]),
                "{key} differs\n rust:   {}\n python: {}",
                ours[key],
                theirs[key]
            );
        }
    }
}

#[test]
fn the_walk_is_the_one_the_python_took() {
    assert_same_walk(&walk(false), &golden("engine/walk.json"));
}

#[test]
fn the_crashing_walk_is_the_one_the_python_took() {
    assert_same_walk(&walk(true), &golden("engine/walk-crash.json"));
}
