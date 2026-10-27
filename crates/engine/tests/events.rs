//! The event bus. What matters most: a subscriber can't stop another from
//! hearing, or undo what was emitted; a provider can't write `sandbox.*`;
//! and events arrive in `sequence` order whichever thread published.

mod support;

use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use beatbox_engine::names::matches;
use beatbox_engine::{Data, EventBus, MockSandbox, Recorder, data, reason};
use support::{config, mock, ticking_clock};

mod pattern_tests {
    use super::*;

    #[test]
    fn a_star_covers_everything() {
        assert!(matches("process.created", "*"));
    }

    #[test]
    fn a_namespace_covers_its_own() {
        assert!(matches("process.created", "process.*"));
        assert!(matches("network.firewall.configured", "network.*"));
        assert!(!matches("filesystem.created", "process.*"));
    }

    /// `process.*` matching `processes.started` is how a subscription
    /// quietly widens into somebody else's events.
    #[test]
    fn a_namespace_does_not_cover_a_longer_word() {
        assert!(!matches("processes.started", "process.*"));
    }

    #[test]
    fn a_plain_name_is_exact() {
        assert!(matches("process.created", "process.created"));
        assert!(!matches("process.exited", "process.created"));
    }

    #[test]
    fn half_a_segment_is_not_a_pattern() {
        assert!(EventBus::default().subscribe("proc*", |_| {}).is_err());
    }
}

mod bus_tests {
    use super::*;

    fn bus() -> EventBus {
        EventBus::new(ticking_clock())
    }

    #[test]
    fn an_event_carries_who_when_and_in_what_order() {
        let bus = bus();
        let first = bus
            .publish("process.created", "sbx-1", data!({"pid": 1}))
            .unwrap();
        let second = bus
            .publish("process.exited", "sbx-1", data!({"pid": 1}))
            .unwrap();
        assert_eq!(
            (first.name(), first.sandbox_id(), first.get("pid").unwrap()),
            ("process.created", "sbx-1", &serde_json::json!(1))
        );
        assert_eq!(second.sequence(), first.sequence() + 1);
        assert!(first.at() < second.at());
    }

    #[test]
    fn the_sequence_is_what_orders_two_events_in_the_same_instant() {
        let frozen = EventBus::new(Arc::new(|| "2026-01-01T00:00:00+00:00".to_owned()));
        let events: Vec<_> = (0..3)
            .map(|_| {
                frozen
                    .publish("process.created", "sbx-1", Data::new())
                    .unwrap()
            })
            .collect();
        assert!(events.iter().all(|event| event.at() == events[0].at()));
        let sequences: Vec<u64> = events.iter().map(|event| event.sequence()).collect();
        assert_eq!(sequences, [1, 2, 3]);
    }

    #[test]
    fn a_subscriber_hears_only_what_it_asked_for() {
        let bus = bus();
        let (everything, processes, exact) = (
            Recorder::default(),
            Recorder::default(),
            Recorder::default(),
        );
        bus.subscribe("*", everything.sink()).unwrap();
        bus.subscribe("process.*", processes.sink()).unwrap();
        bus.subscribe("file.created", exact.sink()).unwrap();
        bus.publish("process.created", "sbx-1", Data::new())
            .unwrap();
        bus.publish("file.created", "sbx-1", Data::new()).unwrap();
        bus.publish("file.modified", "sbx-1", Data::new()).unwrap();
        assert_eq!(everything.len(), 3);
        assert_eq!(processes.names(), ["process.created"]);
        assert_eq!(exact.names(), ["file.created"]);
    }

    #[test]
    fn a_subscriber_that_throws_cannot_take_the_others_with_it() {
        let bus = bus();
        let after = Recorder::default();
        bus.subscribe("*", |_| panic!("the logger is having a day"))
            .unwrap();
        bus.subscribe("*", after.sink()).unwrap();
        let event = bus
            .publish("process.created", "sbx-1", Data::new())
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(event.name(), "process.created");
    }

    #[test]
    fn unsubscribing_stops_the_delivery_and_can_be_done_twice() {
        let bus = bus();
        let log = Recorder::default();
        let subscription = bus.subscribe("*", log.sink()).unwrap();
        bus.publish("process.created", "sbx-1", Data::new())
            .unwrap();
        assert!(bus.unsubscribe(&subscription));
        bus.publish("process.exited", "sbx-1", Data::new()).unwrap();
        assert_eq!(log.names(), ["process.created"]);
        assert!(!bus.unsubscribe(&subscription));
    }

    #[test]
    fn the_same_handler_twice_is_two_subscriptions() {
        let bus = bus();
        let log = Recorder::default();
        let first = bus.subscribe("*", log.sink()).unwrap();
        bus.subscribe("*", log.sink()).unwrap();
        bus.unsubscribe(&first);
        bus.publish("process.created", "sbx-1", Data::new())
            .unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(bus.subscriptions().len(), 1);
    }

    #[test]
    fn an_event_name_has_to_look_like_one() {
        assert!(bus().publish("Started", "sbx-1", Data::new()).is_err());
    }

    /// Depth-first: an event published from inside a handler gets the next
    /// number and is delivered before its parent's delivery carries on.
    #[test]
    fn a_handler_that_publishes_is_delivered_depth_first() {
        let bus = Arc::new(bus());
        let order = Arc::new(Mutex::new(Vec::new()));
        let inner = Arc::clone(&bus);
        bus.subscribe("process.created", move |_| {
            inner
                .publish("process.exited", "sbx-1", Data::new())
                .unwrap();
        })
        .unwrap();
        let seen = Arc::clone(&order);
        bus.subscribe("*", move |event| {
            seen.lock()
                .unwrap()
                .push((event.sequence(), event.name().to_owned()));
        })
        .unwrap();
        bus.publish("process.created", "sbx-1", Data::new())
            .unwrap();
        assert_eq!(
            *order.lock().unwrap(),
            [
                (2, "process.exited".to_owned()),
                (1, "process.created".to_owned())
            ]
        );
    }

    /// Two lanes publishing at once can't interleave a delivery.
    #[test]
    fn a_subscriber_hears_every_thread_in_sequence_order() {
        let bus = Arc::new(bus());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let heard = Arc::clone(&seen);
        bus.subscribe("*", move |event| {
            heard.lock().unwrap().push(event.sequence());
            thread::yield_now();
        })
        .unwrap();
        let start = Arc::new(Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|lane| {
                let (bus, start) = (Arc::clone(&bus), Arc::clone(&start));
                thread::spawn(move || {
                    start.wait();
                    for _ in 0..250 {
                        bus.publish("process.created", &format!("sbx-{lane}"), Data::new())
                            .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1000);
        assert!(
            seen.windows(2).all(|pair| pair[0] < pair[1]),
            "out of order"
        );
    }
}

mod recorder_tests {
    use super::*;

    #[test]
    fn it_keeps_the_newest_when_it_fills_up() {
        let log = Recorder::new(2);
        let bus = EventBus::new(ticking_clock());
        bus.subscribe("*", log.sink()).unwrap();
        for index in 0..4 {
            bus.publish("process.created", "sbx-1", data!({"pid": index}))
                .unwrap();
        }
        let pids: Vec<_> = log
            .events()
            .iter()
            .map(|e| e.get("pid").cloned().unwrap())
            .collect();
        assert_eq!(pids, [2, 3]);
    }
}

/// `sandbox.*` has one author.
mod namespace_tests {
    use super::*;

    #[test]
    fn a_provider_cannot_write_the_lifecycle_story() {
        let (runtime, _log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        let said = sandbox
            .provider(|p: &mut MockSandbox| p.emitter().emit("sandbox.started", Data::new()))
            .unwrap();
        assert!(said.is_err());
    }

    /// The guard lives in the emitter, so a copy used from anywhere is
    /// refused the same way.
    #[test]
    fn a_provider_cannot_get_around_it_by_using_the_raw_emitter() {
        let (runtime, _log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        let emitter = sandbox
            .provider(|p: &mut MockSandbox| p.emitter().clone())
            .unwrap();
        let from_elsewhere = thread::spawn(move || emitter.emit("sandbox.destroyed", Data::new()));
        assert!(from_elsewhere.join().unwrap().is_err());
    }

    #[test]
    fn the_runtime_itself_writes_it() {
        let (runtime, log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        sandbox.start().unwrap();
        let said: Vec<String> = log
            .matching("sandbox.*")
            .iter()
            .map(|e| e.name().to_owned())
            .collect();
        assert_eq!(said, ["sandbox.created", "sandbox.started"]);
        assert!(
            log.events()
                .iter()
                .all(|event| event.sandbox_id() == sandbox.id())
        );
    }

    /// The same trap, one layer down. *"QEMU exited with {code}"* is a
    /// sentence somebody will write, and a value called `code` has to reach
    /// it.
    #[test]
    fn a_reason_can_carry_a_value_called_code() {
        let why = reason!("host.guest_went", code = "137");
        assert_eq!(why.code, "host.guest_went");
        assert_eq!(why.values.len(), 1);
        assert_eq!(why.values["code"], "137");
    }

    /// `emit("snapshot.created", {"name": ...})` is the most natural line a
    /// provider will write. It has to work.
    #[test]
    fn a_provider_can_emit_data_called_name() {
        let (runtime, log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        sandbox
            .provider(|p: &mut MockSandbox| {
                p.emitter().emit(
                    "file.created",
                    data!({"name": "whatever", "path": "/tmp/a"}),
                )
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            log.matching("file.created")[0].data(),
            &data!({"name": "whatever", "path": "/tmp/a"})
        );
    }

    #[test]
    fn what_a_provider_says_is_carried_without_being_read() {
        let (runtime, log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        sandbox.start().unwrap();
        sandbox
            .act(
                "filesystem.write",
                data!({"path": "/tmp/a", "content": "hello"}),
            )
            .unwrap();
        let written = log.matching("file.created");
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].data(), &data!({"path": "/tmp/a", "bytes": 5}));
    }

    /// A subscriber may read the sandbox mid-operation, as the API does to
    /// save it on every change. Without the view this would deadlock.
    #[test]
    fn a_subscriber_can_look_at_the_sandbox_while_it_is_being_moved() {
        let (runtime, _log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        let looked = Arc::new(Mutex::new(Vec::new()));
        let (seen, watched) = (Arc::clone(&looked), sandbox.clone());
        runtime
            .events()
            .subscribe("sandbox.*", move |_| {
                seen.lock().unwrap().push(watched.lifecycle().as_str());
            })
            .unwrap();
        sandbox.start().unwrap();
        sandbox.stop().unwrap();
        assert_eq!(*looked.lock().unwrap(), ["running", "stopped"]);
    }
}
