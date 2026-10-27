//! The proof: one sandbox, driven the whole way through the contract.
//!
//! The headline is `contract_walk_tests::the_walk`. Around it: a failed
//! creation leaves nothing, the boundary is never weaker than asked, nobody
//! edits a configuration behind your back, capabilities only narrow.

mod support;

use std::sync::{Arc, Mutex};

use beatbox_engine::runtime::resources::CPU;
use beatbox_engine::{
    Action, CapabilitySet, EngineError, Isolation, IsolationPolicy, Lifecycle, MockSandbox,
    Observation, ProviderError, ResourceLedger, Resources, SandboxConfig, SandboxIdentity, data,
};
use serde_json::json;
use support::{Hooks, Plain, build, config, mock, plain, plain_with};

fn cores(n: f64) -> Resources {
    Resources::new([(CPU, n)]).unwrap()
}

fn budget(n: f64) -> Option<ResourceLedger> {
    Some(ResourceLedger::with_budget(cores(n)))
}

mod contract_walk_tests {
    use super::*;

    /// Create -> Configure -> Start -> Action -> Event -> Stop -> Destroy.
    #[test]
    fn the_walk() {
        let (runtime, log) = mock();
        let configured = config("mock")
            .named("windows-client")
            .with_resources(
                Resources::new([
                    (CPU, 4.0),
                    (beatbox_engine::runtime::resources::MEMORY, 8192.0),
                ])
                .unwrap(),
            )
            .with_settings(data!({"files": {"/etc/hosts": "127.0.0.1 localhost"}}));
        let sandbox = runtime.create(configured).unwrap();

        assert_eq!(sandbox.identity().type_name, "mock");
        assert_eq!(sandbox.identity().name, "windows-client");
        assert_eq!(sandbox.lifecycle(), Lifecycle::Created);
        assert_eq!(runtime.resources().allocated(&sandbox.id()).get(CPU), 4.0);

        sandbox.start().unwrap();
        assert_eq!(sandbox.lifecycle(), Lifecycle::Running);

        let ran = sandbox
            .act("process.execute", data!({"command": "/tmp/sample.exe"}))
            .unwrap();
        assert!(ran.ok);
        assert_eq!(ran.data["command"], "/tmp/sample.exe");

        let wrote = sandbox
            .act(
                "filesystem.write",
                data!({"path": "/tmp/note", "content": "seen"}),
            )
            .unwrap();
        let read = sandbox
            .act("filesystem.read", data!({"path": "/tmp/note"}))
            .unwrap();
        assert!(wrote.ok);
        assert_eq!(read.data["content"], "seen");

        sandbox.stop().unwrap();
        sandbox.destroy().unwrap();

        assert_eq!(sandbox.lifecycle(), Lifecycle::Destroyed);
        assert!(runtime.resources().committed().is_empty());
        assert!(runtime.sandboxes().is_empty());
        assert!(
            runtime.get(&sandbox.id()).unwrap().same(&sandbox),
            "destroyed is not forgotten"
        );

        assert_eq!(
            log.names(),
            [
                "sandbox.created",
                "process.created", // the mock's init, during start
                "sandbox.started",
                "process.created", // the sample
                "process.exited",
                "sandbox.action",
                "file.created",
                "sandbox.action",
                "sandbox.action", // the read
                "process.exited", // init, killed by the stop
                "sandbox.stopped",
                "sandbox.destroyed",
            ]
        );
    }

    /// `process.created` before `sandbox.started` is right: the provider
    /// spawned it during start, before the transition completed.
    #[test]
    fn the_events_are_in_the_order_things_actually_happened() {
        let (runtime, log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        sandbox.start().unwrap();
        let names = log.names();
        let at = |name: &str| names.iter().position(|n| n == name).unwrap();
        assert!(at("process.created") < at("sandbox.started"));
    }
}

mod creation_tests {
    use super::*;

    #[test]
    fn a_creation_that_fails_leaves_nothing_behind() {
        let cleaned = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&cleaned);
        let brittle = plain_with(
            "brittle",
            &[],
            Isolation::None,
            Hooks {
                create: Some(Arc::new(|_| Err(ProviderError::default()))),
                destroy: Some(Arc::new(move |identity: &SandboxIdentity| {
                    seen.lock().unwrap().push(identity.id.clone());
                    Ok(())
                })),
                ..Hooks::default()
            },
        );
        let (runtime, log) = build(vec![brittle], budget(8.0));
        let made = runtime.create(config("brittle").with_resources(cores(8.0)));
        assert!(matches!(made, Err(EngineError::Provider(_))));

        assert_eq!(
            cleaned.lock().unwrap().len(),
            1,
            "the provider was told to clean up after itself"
        );
        assert!(
            runtime.resources().committed().is_empty(),
            "and the allocation went back"
        );
        assert!(runtime.every_sandbox().is_empty());
        assert_eq!(log.names(), ["sandbox.refused"]);
    }

    #[test]
    fn a_creation_that_fails_does_not_stop_the_next_one() {
        let (runtime, _) = build(vec![], budget(4.0));
        let refused = runtime.create(
            config("mock")
                .with_resources(cores(4.0))
                .with_settings(data!({"refuse": ["create"]})),
        );
        assert!(matches!(refused, Err(EngineError::Provider(_))));
        let sandbox = runtime
            .create(config("mock").with_resources(cores(4.0)))
            .unwrap();
        assert_eq!(sandbox.lifecycle(), Lifecycle::Created);
    }

    #[test]
    fn a_sandbox_is_named_after_its_type_when_it_is_not_named() {
        let (runtime, _) = mock();
        assert_eq!(runtime.create(config("mock")).unwrap().name(), "mock");
    }

    #[test]
    fn every_sandbox_gets_its_own_id() {
        let (runtime, _) = mock();
        let made: std::collections::HashSet<String> = (0..3)
            .map(|_| runtime.create(config("mock")).unwrap().id())
            .collect();
        assert_eq!(made.len(), 3);
    }

    #[test]
    fn asking_about_a_sandbox_that_was_never_made_is_a_bug() {
        let (runtime, _) = mock();
        assert!(matches!(
            runtime.get("sbx-nobody"),
            Err(beatbox_engine::ContractError::UnknownSandbox(_))
        ));
    }
}

/// The one check the runtime makes about what a sandbox is *for*.
mod isolation_tests {
    use super::*;
    use beatbox_engine::runtime::isolation::at_least;

    #[test]
    fn a_weaker_boundary_than_was_asked_for_is_refused() {
        let (runtime, _) = mock();
        let made =
            runtime.create(config("mock").with_isolation(IsolationPolicy::at(Isolation::Machine)));
        let Err(EngineError::Refused(why)) = made else {
            panic!("a weaker boundary was handed out");
        };
        assert_eq!(why.code, "engine.isolation_too_weak");
        assert_eq!(why.values["type"], "mock");
        assert_eq!(why.values["provided"], "none");
        assert_eq!(why.values["required"], "machine");
        assert_eq!(why.values.len(), 3);
    }

    /// The comparison is one-way: it is the only direction in which a
    /// surprise is safe.
    #[test]
    fn a_stronger_one_than_was_asked_for_is_fine() {
        let guest = plain_with("guest", &[], Isolation::Machine, Hooks::default());
        let (runtime, _) = build(vec![guest], None);
        let sandbox = runtime
            .create(config("guest").with_isolation(IsolationPolicy::at(Isolation::Container)))
            .unwrap();
        assert_eq!(sandbox.lifecycle(), Lifecycle::Created);
    }

    #[test]
    fn the_levels_are_ranked_the_way_the_product_talks_about_them() {
        assert!(at_least(Isolation::Machine, Isolation::Container));
        assert!(at_least(Isolation::Container, Isolation::None));
        assert!(!at_least(Isolation::Container, Isolation::Machine));
    }

    #[test]
    fn an_isolation_level_nobody_can_rank_is_refused_at_the_policy() {
        assert!(IsolationPolicy::from_value(&json!({"level": "strict"})).is_err());
    }
}

/// What was asked for, held apart from what is happening.
mod configuration_tests {
    use super::*;

    #[test]
    fn a_configuration_cannot_be_edited_from_outside_after_the_fact() {
        let mut settings = data!({"files": {"/etc/hosts": "127.0.0.1"}});
        let configured = config("mock").with_settings(settings.clone());
        settings["files"]["/etc/shadow"] = json!("root:x");
        assert_eq!(
            configured.settings["files"],
            json!({"/etc/hosts": "127.0.0.1"})
        );
    }

    #[test]
    fn a_provider_cannot_edit_the_configuration_it_was_built_from() {
        let (runtime, _) = mock();
        let sandbox = runtime
            .create(config("mock").with_settings(data!({"files": {"/etc/hosts": "x"}})))
            .unwrap();
        sandbox.start().unwrap();
        sandbox
            .act(
                "filesystem.write",
                data!({"path": "/etc/hosts", "content": "rewritten"}),
            )
            .unwrap();
        assert_eq!(
            sandbox.config().settings["files"],
            json!({"/etc/hosts": "x"})
        );
    }

    #[test]
    fn the_same_configuration_makes_a_second_sandbox() {
        let (runtime, _) = mock();
        let configured = config("mock").named("twin");
        let first = runtime.create(configured.clone()).unwrap();
        let second = runtime.create(configured).unwrap();
        assert_ne!(first.id(), second.id());
        assert_eq!(first.config(), second.config());
    }

    #[test]
    fn a_configuration_survives_being_written_down_and_read_back() {
        let mut policy = IsolationPolicy::at(Isolation::None);
        policy.options = data!({"network": "none"});
        let configured = config("mock")
            .named("windows-client")
            .with_resources(cores(4.0))
            .with_isolation(policy)
            .with_settings(data!({"environment": {"hostname": "WIN-01"}}));
        assert_eq!(
            SandboxConfig::from_value(&configured.to_value()).unwrap(),
            configured
        );
    }

    #[test]
    fn a_configuration_has_to_say_what_it_is_configuring() {
        assert!(SandboxConfig::new("").is_err());
        assert!(SandboxConfig::new("   ").is_err());
    }

    #[test]
    fn desired_and_actual_are_two_different_readings() {
        let (runtime, _) = mock();
        let sandbox = runtime
            .create(config("mock").with_resources(cores(4.0)))
            .unwrap();
        sandbox.start().unwrap();
        assert_eq!(
            sandbox.config().resources.get(CPU),
            4.0,
            "what was asked for"
        );
        assert_eq!(
            sandbox.state().resources.get(CPU),
            0.25,
            "what is being used"
        );
    }
}

mod action_tests {
    use super::*;

    fn running() -> (
        beatbox_engine::SandboxRuntime,
        beatbox_engine::Recorder,
        beatbox_engine::Sandbox,
    ) {
        let (runtime, log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        sandbox.start().unwrap();
        (runtime, log, sandbox)
    }

    #[test]
    fn an_action_names_the_capability_it_needs() {
        let (_r, _l, sandbox) = running();
        let outcome = sandbox
            .perform(Action::new("process.execute", data!({"command": "/bin/true"})).unwrap());
        assert_eq!(outcome.capability(), "process.execute");
        assert!(outcome.ok);
    }

    #[test]
    fn a_capability_that_was_never_declared_is_refused_before_the_provider() {
        let (_r, log, sandbox) = running();
        let outcome = sandbox.act("traffic.capture", data!({})).unwrap();
        assert!(!outcome.ok);
        assert_eq!(outcome.reason.unwrap().code, "engine.unsupported");
        assert!(log.names().contains(&"sandbox.refused".to_owned()));
    }

    /// The distinction the whole outcome shape exists for.
    #[test]
    fn a_program_that_ran_and_failed_is_not_a_refusal() {
        let (_r, _l, sandbox) = running();
        let outcome = sandbox
            .act(
                "process.execute",
                data!({"command": "/bin/false", "exit_code": 1}),
            )
            .unwrap();
        assert!(outcome.ok);
        assert_eq!(outcome.data["exit_code"], 1);
    }

    #[test]
    fn something_the_provider_could_not_do_comes_back_as_data_not_an_exception() {
        let (_r, _l, sandbox) = running();
        let outcome = sandbox
            .act("filesystem.read", data!({"path": "/nowhere"}))
            .unwrap();
        assert!(!outcome.ok);
        let why = outcome.reason.unwrap();
        assert_eq!(why.code, "engine.no_such_file");
        assert_eq!(why.values["path"], "/nowhere");
        assert_eq!(why.values.len(), 1);
    }

    #[test]
    fn a_missing_argument_is_a_named_refusal_rather_than_a_traceback() {
        let (_r, _l, sandbox) = running();
        let outcome = sandbox.act("filesystem.read", data!({})).unwrap();
        assert_eq!(outcome.reason.unwrap().code, "engine.missing_argument");
    }

    /// A provider's panic is wrapped and named, not let loose.
    #[test]
    fn a_provider_that_raises_something_unexpected_is_wrapped_not_leaked() {
        let rude = plain_with(
            "rude",
            &["process.execute"],
            Isolation::None,
            Hooks {
                perform: Some(Arc::new(|_| panic!("no"))),
                ..Hooks::default()
            },
        );
        let (runtime, _) = build(vec![rude], None);
        let sandbox = runtime.create(config("rude")).unwrap();
        sandbox.start().unwrap();
        let outcome = sandbox
            .act("process.execute", data!({"command": "x"}))
            .unwrap();
        assert!(!outcome.ok);
        let why = outcome.reason.unwrap();
        assert_eq!(why.code, "engine.provider_failed");
        assert_eq!(why.values["detail"], "panic: no");
        assert_eq!(why.values["step"], "process.execute");
    }

    /// The other order would say the sandbox died before the thing that
    /// killed it was attempted.
    #[test]
    fn an_action_that_loses_the_sandbox_is_recorded_before_the_loss() {
        let deadly = plain_with(
            "deadly",
            &["process.execute"],
            Isolation::None,
            Hooks {
                perform: Some(Arc::new(|_| {
                    Err(ProviderError {
                        reason: None,
                        fatal: true,
                    })
                })),
                ..Hooks::default()
            },
        );
        let (runtime, log) = build(vec![deadly], None);
        let sandbox = runtime.create(config("deadly")).unwrap();
        sandbox.start().unwrap();
        let outcome = sandbox
            .act("process.execute", data!({"command": "/tmp/bad"}))
            .unwrap();

        assert!(!outcome.ok);
        assert_eq!(sandbox.lifecycle(), Lifecycle::Failed);
        let names = log.names();
        assert_eq!(
            names[names.len() - 2..],
            ["sandbox.action", "sandbox.failed"]
        );
    }

    #[test]
    fn a_sandbox_that_is_gone_cannot_be_asked_to_do_anything() {
        let (_r, _l, sandbox) = running();
        sandbox.destroy().unwrap();
        let outcome = sandbox
            .act("process.execute", data!({"command": "/bin/true"}))
            .unwrap();
        assert_eq!(outcome.reason.unwrap().code, "engine.cannot_act");
    }

    /// The runtime does not know what a capability needs in order to work,
    /// so it does not pretend to. The mock does know.
    #[test]
    fn whether_now_is_a_sensible_moment_is_the_providers_call() {
        let (_r, _l, sandbox) = running();
        sandbox.stop().unwrap();
        let outcome = sandbox
            .act("process.execute", data!({"command": "/bin/true"}))
            .unwrap();
        assert!(!outcome.ok);
        assert_eq!(outcome.reason.unwrap().code, "engine.not_running");
    }

    #[test]
    fn a_snapshot_is_an_action_and_not_a_lifecycle_verb() {
        let (_r, _l, sandbox) = running();
        let act = |capability: &str, arguments| sandbox.act(capability, arguments).unwrap();
        assert!(
            act(
                "filesystem.write",
                data!({"path": "/tmp/before", "content": "1"})
            )
            .ok
        );
        assert!(act("snapshot.create", data!({"name": "before_attack"})).ok);
        assert!(
            act(
                "filesystem.write",
                data!({"path": "/tmp/after", "content": "2"})
            )
            .ok
        );
        assert_eq!(
            act("filesystem.list", data!({})).data["paths"],
            json!(["/tmp/after", "/tmp/before"])
        );

        assert!(act("snapshot.restore", data!({"name": "before_attack"})).ok);
        assert_eq!(
            act("filesystem.list", data!({})).data["paths"],
            json!(["/tmp/before"])
        );
        assert!(!act("snapshot.restore", data!({"name": "never"})).ok);
    }

    /// Including `name` and `capability`, which clash with field names.
    #[test]
    fn an_action_argument_may_be_called_anything() {
        let (_r, log, sandbox) = running();
        assert!(
            sandbox
                .act(
                    "snapshot.create",
                    data!({"name": "whatever", "capability": "x"})
                )
                .unwrap()
                .ok
        );
        assert_eq!(
            log.matching("snapshot.created")[0].data(),
            &data!({"name": "whatever", "files": 0})
        );
    }
}

mod capability_tests {
    use super::*;

    #[test]
    fn a_sandbox_can_declare_less_than_its_type_but_never_more() {
        let overreaching = plain_with(
            "overreaching",
            &["process.execute"],
            Isolation::None,
            Hooks {
                capabilities: Some(Arc::new(|_| {
                    CapabilitySet::new(["process.execute", "snapshot.create"]).unwrap()
                })),
                ..Hooks::default()
            },
        );
        let (runtime, _) = build(vec![overreaching], None);
        let sandbox = runtime.create(config("overreaching")).unwrap();
        assert_eq!(sandbox.capabilities().to_list(), ["process.execute"]);
    }

    #[test]
    fn narrowing_is_what_a_configuration_without_a_disk_does() {
        let diskless = plain_with(
            "diskless",
            &["process.execute", "snapshot.create"],
            Isolation::None,
            Hooks {
                capabilities: Some(Arc::new(|configured: &SandboxConfig| {
                    if configured.setting("disk").is_none() {
                        CapabilitySet::new(["process.execute"]).unwrap()
                    } else {
                        CapabilitySet::new(["process.execute", "snapshot.create"]).unwrap()
                    }
                })),
                ..Hooks::default()
            },
        );
        let (runtime, _) = build(vec![diskless], None);
        let without = runtime.create(config("diskless")).unwrap();
        let with_disk = runtime
            .create(config("diskless").with_settings(data!({"disk": "/tmp/a.qcow2"})))
            .unwrap();
        assert_eq!(without.capabilities().to_list(), ["process.execute"]);
        assert_eq!(
            with_disk.capabilities().to_list(),
            ["process.execute", "snapshot.create"]
        );
    }
}

mod collection_tests {
    use super::*;

    fn three() -> (beatbox_engine::SandboxRuntime, Vec<beatbox_engine::Sandbox>) {
        let (runtime, _) = mock();
        let boxes = (0..3)
            .map(|n| {
                runtime
                    .create(config("mock").named(&format!("box-{n}")))
                    .unwrap()
            })
            .collect();
        (runtime, boxes)
    }

    #[test]
    fn sandboxes_come_back_in_the_order_they_were_made() {
        let (runtime, _) = three();
        let names: Vec<String> = runtime.sandboxes().iter().map(|s| s.name()).collect();
        assert_eq!(names, ["box-0", "box-1", "box-2"]);
    }

    #[test]
    fn a_destroyed_sandbox_is_not_listed_but_is_still_answerable() {
        let (runtime, boxes) = three();
        boxes[1].destroy().unwrap();
        let names: Vec<String> = runtime.sandboxes().iter().map(|s| s.name()).collect();
        assert_eq!(names, ["box-0", "box-2"]);
        assert_eq!(runtime.every_sandbox().len(), 3);
        assert_eq!(
            runtime.get(&boxes[1].id()).unwrap().lifecycle(),
            Lifecycle::Destroyed
        );
    }

    /// Ask for the capability, not for a type name.
    #[test]
    fn who_here_could_run_a_process() {
        let (runtime, boxes) = three();
        boxes[0].start().unwrap();
        assert_eq!(runtime.providing("process.execute", false).len(), 3);
        let now = runtime.providing("process.execute", true);
        assert_eq!(now.len(), 1);
        assert!(now[0].same(&boxes[0]));
        assert!(runtime.providing("traffic.capture", false).is_empty());
    }

    #[test]
    fn taking_everything_down_takes_the_newest_first() {
        let (runtime, boxes) = three();
        let order = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&order);
        runtime
            .events()
            .subscribe("sandbox.destroyed", move |event| {
                seen.lock().unwrap().push(event.sandbox_id().to_owned());
            })
            .unwrap();
        assert!(runtime.destroy_all().is_empty());
        let newest_first: Vec<String> = boxes.iter().rev().map(|s| s.id()).collect();
        assert_eq!(*order.lock().unwrap(), newest_first);
        assert!(runtime.sandboxes().is_empty());
    }

    #[test]
    fn one_stubborn_sandbox_does_not_strand_the_rest() {
        let stubborn = plain_with(
            "stubborn",
            &[],
            Isolation::None,
            Hooks {
                destroy: Some(Arc::new(|_| Err(ProviderError::default()))),
                ..Hooks::default()
            },
        );
        let (runtime, _) = build(vec![MockSandbox::spec(), stubborn], None);
        let stuck = runtime.create(config("stubborn")).unwrap();
        let ordinary = runtime.create(config("mock")).unwrap();
        assert_eq!(runtime.destroy_all(), [stuck.id()]);
        assert_eq!(ordinary.lifecycle(), Lifecycle::Destroyed);
    }
}

/// `restore()`: bringing back a sandbox this runtime did not create.
mod restore_tests {
    use super::*;

    fn identity() -> SandboxIdentity {
        SandboxIdentity::new("sbx-1", "plain", "x", "1")
    }

    #[test]
    fn the_same_identity_comes_back() {
        let (runtime, _) = mock();
        let made = runtime.create(config("mock").named("original")).unwrap();

        let (second, _) = mock();
        let restored = second
            .restore(made.identity(), made.config(), Lifecycle::Created, None)
            .unwrap();

        assert!(restored.same(&second.get(&made.id()).unwrap()));
        assert_eq!(restored.id(), made.id());
        assert_eq!(restored.name(), "original");
        assert_eq!(restored.capabilities(), made.capabilities());
    }

    /// `create()` on a host makes a fresh overlay, which restoring must not.
    #[test]
    fn it_does_not_call_create_on_the_provider() {
        let (runtime, _) = build(vec![plain()], None);
        let restored = runtime
            .restore(identity(), config("plain"), Lifecycle::Created, None)
            .unwrap();
        let calls = restored.provider(|p: &mut Plain| p.calls.clone()).unwrap();
        assert!(!calls.contains(&"create".to_owned()));
    }

    #[test]
    fn the_hinted_lifecycle_is_kept_when_the_provider_agrees_it_is_alive() {
        let (runtime, _) = build(vec![plain()], None);
        let restored = runtime
            .restore(identity(), config("plain"), Lifecycle::Stopped, None)
            .unwrap();
        assert_eq!(restored.lifecycle(), Lifecycle::Stopped);
    }

    /// The hint is a starting guess that `observe()` corrects.
    #[test]
    fn a_provider_that_finds_nothing_fails_regardless_of_the_hint() {
        let gone = plain_with(
            "gone",
            &[],
            Isolation::None,
            Hooks {
                observe: Some(Arc::new(|| Observation {
                    alive: Some(false),
                    ..Observation::default()
                })),
                ..Hooks::default()
            },
        );
        let (runtime, log) = build(vec![plain(), gone], None);
        let restored = runtime
            .restore(
                SandboxIdentity::new("sbx-1", "gone", "x", "1"),
                config("gone"),
                Lifecycle::Running,
                None,
            )
            .unwrap();
        assert_eq!(restored.lifecycle(), Lifecycle::Failed);
        assert!(!log.matching("sandbox.failed").is_empty());
    }

    #[test]
    fn the_same_id_twice_is_refused() {
        let (runtime, _) = build(vec![plain()], None);
        runtime
            .restore(identity(), config("plain"), Lifecycle::Created, None)
            .unwrap();
        assert!(matches!(
            runtime.restore(identity(), config("plain"), Lifecycle::Created, None),
            Err(EngineError::Contract(_))
        ));
    }

    #[test]
    fn the_resources_it_asked_for_are_allocated_again() {
        let (runtime, _) = build(vec![plain()], budget(4.0));
        runtime
            .restore(
                identity(),
                config("plain").with_resources(cores(3.0)),
                Lifecycle::Created,
                None,
            )
            .unwrap();
        assert_eq!(runtime.resources().available().unwrap().get(CPU), 1.0);
    }

    #[test]
    fn an_unregistered_type_is_refused_by_name() {
        let (runtime, _) = build(vec![plain()], None);
        let restored = runtime.restore(
            SandboxIdentity::new("sbx-1", "nothing_like_that", "x", "1"),
            config("nothing_like_that"),
            Lifecycle::Created,
            None,
        );
        assert!(matches!(restored, Err(EngineError::Refused(_))));
    }
}

/// A new configuration for a sandbox that is not running -- given whole,
/// never edited, and all-or-nothing.
mod reconfigure_tests {
    use super::*;

    fn set_up() -> (
        beatbox_engine::SandboxRuntime,
        beatbox_engine::Recorder,
        beatbox_engine::Sandbox,
    ) {
        let (runtime, log) = build(vec![], budget(4.0));
        let sandbox = runtime
            .create(
                config("mock")
                    .with_resources(cores(1.0))
                    .with_settings(data!({"files": {"/a": "1"}})),
            )
            .unwrap();
        (runtime, log, sandbox)
    }

    fn other(cpu: f64, settings: beatbox_engine::Data) -> SandboxConfig {
        config("mock")
            .with_resources(cores(cpu))
            .with_settings(settings)
    }

    #[test]
    fn it_is_replaced_and_said_so() {
        let (runtime, log, sandbox) = set_up();
        sandbox
            .reconfigure(other(3.0, data!({"files": {"/b": "2"}})))
            .unwrap();
        assert_eq!(sandbox.config().resources.get(CPU), 3.0);
        assert_eq!(runtime.resources().available().unwrap().get(CPU), 1.0);
        assert!(!log.matching("sandbox.reconfigured").is_empty());
        assert_eq!(
            sandbox.lifecycle(),
            Lifecycle::Created,
            "not a lifecycle transition"
        );
    }

    #[test]
    fn not_while_it_runs() {
        let (_runtime, _log, sandbox) = set_up();
        sandbox.start().unwrap();
        let Err(EngineError::Refused(why)) = sandbox.reconfigure(other(2.0, data!({}))) else {
            panic!("a running sandbox was reconfigured");
        };
        assert_eq!(why.code, "engine.reconfigure_not_now");
    }

    #[test]
    fn a_budget_that_cannot_cover_it_changes_nothing() {
        let (runtime, _log, sandbox) = set_up();
        assert!(matches!(
            sandbox.reconfigure(other(9.0, data!({}))),
            Err(EngineError::Refused(_))
        ));
        assert_eq!(sandbox.config().resources.get(CPU), 1.0);
        assert_eq!(
            runtime.resources().available().unwrap().get(CPU),
            3.0,
            "the old allocation is back"
        );
    }

    #[test]
    fn a_provider_refusal_changes_nothing() {
        let (runtime, _log, _sandbox) = set_up();
        let sandbox = runtime
            .create(config("mock").with_settings(data!({"refuse": ["reconfigure"]})))
            .unwrap();
        assert!(matches!(
            sandbox.reconfigure(config("mock").with_settings(data!({"refuse": []}))),
            Err(EngineError::Provider(_))
        ));
        assert_eq!(
            sandbox.config().setting("refuse").unwrap(),
            &json!(["reconfigure"])
        );
    }

    #[test]
    fn it_cannot_become_another_type() {
        let (runtime, _) = build(vec![MockSandbox::spec(), plain()], None);
        let sandbox = runtime.create(config("mock")).unwrap();
        assert!(matches!(
            sandbox.reconfigure(config("plain")),
            Err(EngineError::Refused(_))
        ));
    }
}
