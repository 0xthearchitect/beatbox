//! The daemon answers each request on its own thread, straight into
//! `Api::dispatch`, and promises:
//!
//! 1. reading never waits;
//! 2. two operations on one sandbox never overlap, and two refreshes queued
//!    behind the same save point are answered by one observation, not two;
//! 3. two sandboxes never wait for each other;
//! 4. every subscriber sees events in `sequence` order.
//!
//! A slow provider stands in for a save point writing a guest's memory: it
//! holds its lane for as long as it is told to.

mod support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use beatbox_api::{Api, ApiParts};
use beatbox_engine::{
    Action, Data, Emitter, Isolation, MockSandbox, Observation, ProviderError, ProviderSpec,
    SandboxConfig, SandboxIdentity, SandboxProvider,
};
use serde_json::{Value, json};
use support::{Room, data, parts};

/// What one slow sandbox has been through, found by its name.
#[derive(Default)]
struct Counters {
    observed: AtomicU64,
    inside: AtomicI32,
    most_inside: AtomicI32,
}

static COUNTERS: LazyLock<Mutex<HashMap<String, Arc<Counters>>>> = LazyLock::new(Mutex::default);

fn counters(name: &str) -> Arc<Counters> {
    Arc::clone(COUNTERS.lock().unwrap().entry(name.to_owned()).or_default())
}

struct Slow {
    counters: Arc<Counters>,
}

impl SandboxProvider for Slow {
    fn create(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    fn start(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    fn stop(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    fn destroy(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    fn observe(&mut self) -> Observation {
        self.counters.observed.fetch_add(1, Ordering::SeqCst);
        Observation {
            alive: Some(true),
            ..Observation::default()
        }
    }

    /// Holds the lane for `ms`, counting how many are inside at once.
    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let now = self.counters.inside.fetch_add(1, Ordering::SeqCst) + 1;
        self.counters.most_inside.fetch_max(now, Ordering::SeqCst);
        let ms = action.argument("ms").and_then(Value::as_u64).unwrap_or(0);
        thread::sleep(Duration::from_millis(ms));
        self.counters.inside.fetch_sub(1, Ordering::SeqCst);
        Ok(Data::new())
    }
}

fn slow_spec() -> ProviderSpec {
    ProviderSpec::new(
        "slow",
        "1",
        &["snapshot.create", "process.execute"],
        Isolation::None,
        |_identity: SandboxIdentity, config: SandboxConfig, _emit: Emitter| {
            Box::new(Slow {
                counters: counters(&config.name),
            })
        },
    )
}

fn api(room: &Room) -> Arc<Api> {
    Arc::new(Api::open(ApiParts {
        types: vec![slow_spec(), MockSandbox::spec()],
        ..parts(room.path())
    }))
}

/// A running sandbox of `kind`, by id.
fn running(api: &Api, kind: &str, name: &str) -> String {
    let made = data(api.dispatch(
        "sandbox.create",
        &json!({"type": kind, "name": name, "cpu": 0, "memory": 0, "settings": {}, "uses": {}}),
    ));
    let id = made["id"].as_str().unwrap().to_owned();
    data(api.dispatch(
        "sandbox.transition",
        &json!({"id": id, "transition": "start"}),
    ));
    id
}

/// Ask `id` to spend `ms` inside its lane, on a thread of its own.
fn hold(api: &Arc<Api>, id: &str, capability: &str, ms: u64) -> thread::JoinHandle<()> {
    let api = Arc::clone(api);
    let payload = json!({"id": id, "capability": capability, "arguments": {"ms": ms}});
    let handle = thread::spawn(move || {
        let outcome = data(api.dispatch("sandbox.act", &payload));
        assert_eq!(outcome["ok"], true, "{outcome}");
    });
    // Long enough for it to be inside.
    thread::sleep(Duration::from_millis(150));
    handle
}

fn timed(work: impl FnOnce()) -> Duration {
    let started = Instant::now();
    work();
    started.elapsed()
}

#[test]
fn reading_never_waits_for_a_lane() {
    let room = Room::new("lanes");
    let api = api(&room);
    let id = running(&api, "slow", "read-while-saving");
    let saving = hold(&api, &id, "snapshot.create", 1500);

    for action in [
        "sandbox.view",
        "sandbox.list",
        "sandbox.types",
        "sandbox.events",
    ] {
        let payload = match action {
            "sandbox.view" => json!({"id": id}),
            "sandbox.events" => json!({"since": 0}),
            _ => json!({}),
        };
        let took = timed(|| {
            data(api.dispatch(action, &payload));
        });
        assert!(
            took < Duration::from_millis(300),
            "{action} waited {took:?}"
        );
    }
    saving.join().unwrap();
}

#[test]
fn two_operations_on_one_sandbox_never_overlap() {
    let room = Room::new("lanes");
    let api = api(&room);
    let id = running(&api, "slow", "one-at-a-time");
    let took = timed(|| {
        let first = hold(&api, &id, "process.execute", 400);
        let second = hold(&api, &id, "process.execute", 400);
        first.join().unwrap();
        second.join().unwrap();
    });
    assert_eq!(
        counters("one-at-a-time").most_inside.load(Ordering::SeqCst),
        1
    );
    assert!(took >= Duration::from_millis(800), "{took:?}");
}

#[test]
fn two_refreshes_behind_one_save_point_are_one_observation() {
    let room = Room::new("lanes");
    let api = api(&room);
    let id = running(&api, "slow", "coalesced");
    let seen = counters("coalesced");
    let before = seen.observed.load(Ordering::SeqCst);

    let saving = hold(&api, &id, "snapshot.create", 800);
    let refreshes: Vec<_> = (0..2)
        .map(|_| {
            let api = Arc::clone(&api);
            let payload = json!({"id": id});
            thread::spawn(move || data(api.dispatch("sandbox.refresh", &payload)))
        })
        .collect();
    saving.join().unwrap();
    for refresh in refreshes {
        assert_eq!(refresh.join().unwrap()["state"]["lifecycle"], "running");
    }
    assert_eq!(
        seen.observed.load(Ordering::SeqCst),
        before + 1,
        "the guest was asked twice"
    );

    // And one asked afterwards is not answered from the past.
    data(api.dispatch("sandbox.refresh", &json!({"id": id})));
    assert_eq!(seen.observed.load(Ordering::SeqCst), before + 2);
}

#[test]
fn two_sandboxes_never_wait_for_each_other() {
    let room = Room::new("lanes");
    let api = api(&room);
    let busy = running(&api, "slow", "busy-one");
    let free = running(&api, "slow", "free-one");
    let saving = hold(&api, &busy, "snapshot.create", 1500);

    let took = timed(|| {
        let outcome = data(api.dispatch(
            "sandbox.act",
            &json!({"id": free, "capability": "process.execute", "arguments": {"ms": 0}}),
        ));
        assert_eq!(outcome["ok"], true);
        data(api.dispatch("sandbox.refresh", &json!({"id": free})));
        data(api.dispatch(
            "sandbox.transition",
            &json!({"id": free, "transition": "stop"}),
        ));
    });
    assert!(took < Duration::from_millis(300), "it waited {took:?}");
    saving.join().unwrap();
}

/// Eight lanes at once, each saving its record on every change: the page
/// hears it all in `sequence` order. Bounded, since a subscriber waiting on
/// a lane would hang here.
#[test]
fn every_subscriber_hears_events_in_sequence_order() {
    let room = Room::new("lanes");
    let api = api(&room);
    let heard = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&heard);
    api.hub().subscribe(move |name, payload| {
        if name == "sandbox" {
            kept.lock()
                .unwrap()
                .push(payload["sequence"].as_u64().unwrap());
        }
    });
    let before = api.runtime().events().published();

    let (done, finished) = mpsc::channel();
    for n in 0..8 {
        let api = Arc::clone(&api);
        let done = done.clone();
        thread::spawn(move || {
            let id = running(&api, "mock", &format!("lane-{n}"));
            for _ in 0..10 {
                for transition in ["pause", "resume", "stop", "start"] {
                    data(api.dispatch(
                        "sandbox.transition",
                        &json!({"id": id, "transition": transition}),
                    ));
                }
                data(api.dispatch("sandbox.refresh", &json!({"id": id})));
            }
            done.send(()).unwrap();
        });
    }
    for _ in 0..8 {
        finished
            .recv_timeout(Duration::from_secs(30))
            .expect("a lane never finished");
    }

    let heard = heard.lock().unwrap();
    assert!(
        heard.windows(2).all(|pair| pair[0] < pair[1]),
        "out of order"
    );
    assert_eq!(
        heard.len() as u64,
        api.runtime().events().published() - before,
        "something was not heard"
    );
}
