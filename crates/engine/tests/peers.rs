//! A sandbox that uses another: roles checked at creation, acted on by
//! capability, fixed for life.
//!
//! Two mocks: the peer, and `relay`, which needs a `box` that runs processes
//! and passes on whatever it's asked.

mod support;

use beatbox_engine::{
    Action, Data, EngineError, Isolation, Lifecycle, MockSandbox, Observation, Peers,
    ProviderError, ProviderSpec, Requirement, SandboxConfig, SandboxProvider, SandboxRegistry,
    data,
};
use serde_json::{Value, json};
use support::{build, config};

#[derive(Default)]
struct Relay {
    peers: Peers,
}

impl SandboxProvider for Relay {
    fn given(&mut self, peers: Peers) {
        self.peers = peers;
    }
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
        Observation::default()
    }
    fn reconfigure(&mut self, _config: &SandboxConfig) -> Result<(), ProviderError> {
        Ok(())
    }
    /// `relay.pass {role, capability, arguments}`: the peer's outcome, as data.
    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        let text = |key: &str| action.argument(key).and_then(Value::as_str).unwrap_or("");
        let arguments = action
            .argument("arguments")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let outcome = self
            .peers
            .act(text("role"), text("capability"), arguments)
            .expect("a valid capability");
        Ok(data!({
            "ok": outcome.ok,
            "data": outcome.data,
            "reason": outcome.reason.map(|why| why.code),
            "peer": self.peers.id("box"),
        }))
    }
}

fn relay_spec(capabilities: &[&str]) -> ProviderSpec {
    ProviderSpec::new(
        "relay",
        "1",
        &["relay.pass", "lifecycle.reconfigure"],
        Isolation::None,
        |_, _, _| Box::new(Relay::default()),
    )
    .with_requires(&[Requirement::new("box", capabilities)])
}

fn world() -> (beatbox_engine::SandboxRuntime, beatbox_engine::Recorder) {
    build(
        vec![
            MockSandbox::spec(),
            relay_spec(&["process.execute", "process.list"]),
        ],
        None,
    )
}

fn pass(relay: &beatbox_engine::Sandbox, capability: &str, arguments: Data) -> Data {
    let outcome = relay
        .act(
            "relay.pass",
            data!({"role": "box", "capability": capability, "arguments": arguments}),
        )
        .unwrap();
    assert!(outcome.ok, "{:?}", outcome.reason);
    outcome.data
}

fn refusal(result: Result<beatbox_engine::Sandbox, EngineError>) -> (String, Data) {
    match result {
        Err(EngineError::Refused(why)) => (
            why.code.clone(),
            why.values
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect(),
        ),
        Err(other) => panic!("not a refusal: {other}"),
        Ok(sandbox) => panic!("{sandbox} was made"),
    }
}

#[test]
fn a_sandbox_acts_on_its_peer_through_the_peers_own_gates() {
    let (runtime, log) = world();
    let peer = runtime.create(config("mock").named("web-01")).unwrap();
    peer.start().unwrap();
    let relay = runtime
        .create(config("relay").using("box", "web-01"))
        .unwrap();
    // Written down by id, so a rename does not lose it.
    assert_eq!(relay.config().uses["box"], peer.id());

    log.clear();
    let ran = pass(&relay, "process.execute", data!({"command": "nginx"}));
    assert_eq!(ran["ok"], true);
    assert_eq!(ran["data"]["command"], "nginx");
    assert_eq!(ran["peer"], json!(peer.id()));
    // The peer's own story: its action, on its own id.
    let peers_action = log
        .events()
        .into_iter()
        .find(|event| event.name() == "sandbox.action" && event.sandbox_id() == peer.id())
        .expect("the peer recorded the action");
    assert_eq!(peers_action.data()["capability"], "process.execute");

    // The peer's gates still apply: a stopped peer is asked and says no.
    peer.stop().unwrap();
    let refused = pass(&relay, "process.execute", data!({"command": "nginx"}));
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["reason"], "engine.not_running");
}

#[test]
fn a_capability_the_role_did_not_name_is_refused() {
    let (runtime, _log) = world();
    runtime.create(config("mock").named("web-01")).unwrap();
    let relay = runtime
        .create(config("relay").using("box", "web-01"))
        .unwrap();
    let wrote = pass(&relay, "filesystem.write", data!({"path": "/a"}));
    assert_eq!(wrote["reason"], "engine.peer_undeclared");
    let outcome = relay
        .act(
            "relay.pass",
            data!({"role": "nobody", "capability": "process.execute"}),
        )
        .unwrap();
    assert_eq!(outcome.data["reason"], "engine.peer_undeclared");
}

#[test]
fn every_role_is_checked_before_anything_is_made() {
    let (runtime, log) = world();
    runtime.create(config("mock").named("web-01")).unwrap();
    runtime.create(config("mock").named("twin")).unwrap();
    runtime.create(config("mock").named("twin")).unwrap();
    log.clear();

    let (code, values) = refusal(runtime.create(config("relay")));
    assert_eq!(
        (code.as_str(), &values["role"]),
        ("engine.peer_missing", &json!("box"))
    );

    let (code, values) = refusal(runtime.create(config("relay").using("box", "nowhere")));
    assert_eq!(code, "engine.peer_unknown");
    assert_eq!(values["target"], "nowhere");

    let (code, _) = refusal(runtime.create(config("relay").using("box", "twin")));
    assert_eq!(code, "engine.peer_unknown", "two called twin: which?");

    let (code, _) = refusal(
        runtime.create(
            config("relay")
                .using("box", "web-01")
                .using("disk", "web-01"),
        ),
    );
    assert_eq!(code, "engine.peer_unexpected");

    assert!(
        log.names().is_empty(),
        "nothing was built: {:?}",
        log.names()
    );
    assert_eq!(runtime.sandboxes().len(), 3);
}

#[test]
fn a_peer_that_cannot_do_what_the_role_needs_is_refused_by_name() {
    let (runtime, _log) = build(
        vec![MockSandbox::spec(), relay_spec(&["network.attach"])],
        None,
    );
    runtime.create(config("mock").named("web-01")).unwrap();
    let (code, values) = refusal(runtime.create(config("relay").using("box", "web-01")));
    assert_eq!(code, "engine.peer_lacks");
    assert_eq!(values["peer"], "web-01");
    assert_eq!(values["capability"], "network.attach");
}

#[test]
fn a_peer_destroyed_is_gone_not_held() {
    let (runtime, _log) = world();
    let peer = runtime.create(config("mock").named("web-01")).unwrap();
    let relay = runtime
        .create(config("relay").using("box", "web-01"))
        .unwrap();
    peer.destroy().unwrap();
    let gone = pass(&relay, "process.list", Data::new());
    assert_eq!(gone["reason"], "engine.peer_gone");
}

#[test]
fn who_fills_a_role_is_fixed_for_life() {
    let (runtime, _log) = world();
    let first = runtime.create(config("mock").named("web-01")).unwrap();
    runtime.create(config("mock").named("web-02")).unwrap();
    let relay = runtime
        .create(config("relay").using("box", "web-01"))
        .unwrap();

    // Saying nothing about it keeps it.
    relay.reconfigure(config("relay").named("renamed")).unwrap();
    assert_eq!(relay.config().uses["box"], first.id());

    let refused = relay
        .reconfigure(config("relay").using("box", "web-02"))
        .unwrap_err();
    let EngineError::Refused(why) = refused else {
        panic!("not a refusal")
    };
    assert_eq!(why.code, "engine.peer_fixed");
    assert_eq!(relay.config().uses["box"], first.id());
}

#[test]
fn brought_back_in_either_order_it_finds_its_peer() {
    let (before, _log) = world();
    let peer = before.create(config("mock").named("web-01")).unwrap();
    let relay = before
        .create(config("relay").using("box", "web-01"))
        .unwrap();
    let saved = SandboxConfig::from_value(&relay.config().to_value()).unwrap();
    assert_eq!(saved.uses, relay.config().uses);

    let (after, _log) = world();
    // The relay first: its peer is not back yet.
    let relay = after
        .restore(relay.identity(), saved, Lifecycle::Created, None)
        .unwrap();
    assert_eq!(
        pass(&relay, "process.list", Data::new())["reason"],
        "engine.peer_gone"
    );
    let peer = after
        .restore(peer.identity(), peer.config(), Lifecycle::Running, None)
        .unwrap();
    let listed = pass(&relay, "process.list", Data::new());
    assert_eq!(listed["ok"], true, "{listed:?}");
    assert_eq!(listed["peer"], json!(peer.id()));
}

#[test]
fn a_configuration_with_no_peers_is_written_as_before() {
    let written = config("mock").named("web-01").to_value();
    assert!(written.get("uses").is_none(), "{written}");
}

#[test]
fn a_type_may_not_require_its_peers_lifecycle_or_a_role_twice() {
    let mut registry = SandboxRegistry::default();
    assert!(registry.register(relay_spec(&["lifecycle.pause"])).is_err());
    let twice = relay_spec(&["process.list"]).with_requires(&[
        Requirement::new("box", &["process.list"]),
        Requirement::new("box", &["process.execute"]),
    ]);
    assert!(registry.register(twice).is_err());
    assert!(registry.register(relay_spec(&["process.list"])).is_ok());
}

mod optional_roles {
    use super::*;

    fn optional_world(
        capabilities: &[&str],
    ) -> (beatbox_engine::SandboxRuntime, beatbox_engine::Recorder) {
        let relay = ProviderSpec::new(
            "relay",
            "1",
            &["relay.pass", "lifecycle.reconfigure"],
            Isolation::None,
            |_, _, _| Box::new(Relay::default()),
        )
        .with_requires(&[Requirement::new("box", capabilities).optional()]);
        build(vec![MockSandbox::spec(), relay], None)
    }

    #[test]
    fn left_empty_it_is_absent_not_gone() {
        let (runtime, _log) = optional_world(&["process.list"]);
        let relay = runtime.create(config("relay")).unwrap();
        assert!(relay.config().uses.is_empty());
        assert!(relay.config().to_value().get("uses").is_none());
        let absent = pass(&relay, "process.list", Data::new());
        assert_eq!(absent["reason"], "engine.peer_absent");
        assert_eq!(absent["peer"], Value::Null);
        // Not a role it has at all: still undeclared, as before.
        let outcome = relay
            .act(
                "relay.pass",
                data!({"role": "nobody", "capability": "process.list"}),
            )
            .unwrap();
        assert_eq!(outcome.data["reason"], "engine.peer_undeclared");
    }

    #[test]
    fn filled_it_is_checked_and_used_like_any_other() {
        let (runtime, _log) = optional_world(&["process.list"]);
        let peer = runtime.create(config("mock").named("web-01")).unwrap();
        let relay = runtime
            .create(config("relay").using("box", "web-01"))
            .unwrap();
        let listed = pass(&relay, "process.list", Data::new());
        assert_eq!(listed["ok"], true, "{listed:?}");
        assert_eq!(listed["peer"], json!(peer.id()));

        let (code, _) = refusal(runtime.create(config("relay").using("box", "nowhere")));
        assert_eq!(code, "engine.peer_unknown");
    }

    #[test]
    fn filled_with_one_that_cannot_do_it_is_refused() {
        let (runtime, _log) = optional_world(&["network.attach"]);
        runtime.create(config("mock").named("web-01")).unwrap();
        let (code, _) = refusal(runtime.create(config("relay").using("box", "web-01")));
        assert_eq!(code, "engine.peer_lacks");
    }

    #[test]
    fn brought_back_empty_it_is_still_absent() {
        let (before, _log) = optional_world(&["process.list"]);
        let relay = before.create(config("relay")).unwrap();
        let saved = SandboxConfig::from_value(&relay.config().to_value()).unwrap();
        let (after, _log) = optional_world(&["process.list"]);
        let relay = after
            .restore(relay.identity(), saved, Lifecycle::Created, None)
            .unwrap();
        assert_eq!(
            pass(&relay, "process.list", Data::new())["reason"],
            "engine.peer_absent"
        );
    }
}

mod reusing {
    use super::*;
    use std::collections::BTreeMap;

    fn uses(role: &str, target: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(role.to_owned(), target.to_owned())])
    }

    #[test]
    fn a_stopped_sandbox_is_pointed_at_another_and_acts_on_it() {
        let (runtime, log) = world();
        let first = runtime.create(config("mock").named("web-01")).unwrap();
        let second = runtime.create(config("mock").named("web-02")).unwrap();
        second.start().unwrap();
        let relay = runtime
            .create(config("relay").using("box", "web-01"))
            .unwrap();
        log.clear();

        runtime.reuse(&relay.id(), &uses("box", "web-02")).unwrap();
        assert_eq!(relay.config().uses["box"], second.id());
        assert_eq!(log.names(), ["sandbox.reconfigured"]);
        // The new peer is the one it acts on now.
        let listed = pass(&relay, "process.list", Data::new());
        assert_eq!(listed["peer"], json!(second.id()));
        assert_eq!(listed["ok"], true, "{listed:?}");
        assert_ne!(listed["peer"], json!(first.id()));
        // And it is what a saved record would say.
        let saved = SandboxConfig::from_value(&relay.config().to_value()).unwrap();
        assert_eq!(saved.uses["box"], second.id());
    }

    #[test]
    fn a_running_sandbox_is_not_pointed_anywhere() {
        let (runtime, _log) = world();
        runtime.create(config("mock").named("web-01")).unwrap();
        runtime.create(config("mock").named("web-02")).unwrap();
        let relay = runtime
            .create(config("relay").using("box", "web-01"))
            .unwrap();
        relay.start().unwrap();
        let Err(EngineError::Refused(why)) = runtime.reuse(&relay.id(), &uses("box", "web-02"))
        else {
            panic!("a running sandbox was re-pointed");
        };
        assert_eq!(why.code, "engine.reconfigure_not_now");
    }

    fn refused_code(result: Result<beatbox_engine::Sandbox, EngineError>) -> String {
        match result {
            Err(EngineError::Refused(why)) => why.code,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn what_creation_refuses_it_refuses_too_and_changes_nothing() {
        let (runtime, _log) = world();
        let peer = runtime.create(config("mock").named("web-01")).unwrap();
        let relay = runtime
            .create(config("relay").using("box", "web-01"))
            .unwrap();
        assert_eq!(
            refused_code(runtime.reuse(&relay.id(), &uses("box", "nowhere"))),
            "engine.peer_unknown"
        );
        assert_eq!(
            refused_code(runtime.reuse(&relay.id(), &uses("disk", "web-01"))),
            "engine.peer_unexpected"
        );
        assert_eq!(
            refused_code(runtime.reuse(&relay.id(), &BTreeMap::new())),
            "engine.peer_missing"
        );
        assert_eq!(relay.config().uses["box"], peer.id(), "still where it was");
    }

    #[test]
    fn never_itself_and_never_a_cycle() {
        let (runtime, _log) = build(
            vec![
                relay_spec(&["relay.pass"]),
                ProviderSpec::new("other", "1", &["relay.pass"], Isolation::None, |_, _, _| {
                    Box::new(Relay::default())
                })
                .with_requires(&[Requirement::new("box", &["relay.pass"]).optional()]),
            ],
            None,
        );
        let a = runtime.create(config("other").named("a")).unwrap();
        let _b = runtime
            .create(config("other").named("b").using("box", "a"))
            .unwrap();
        assert_eq!(
            refused_code(runtime.reuse(&a.id(), &uses("box", "a"))),
            "engine.peer_self"
        );
        // b uses a, so a may not use b.
        assert_eq!(
            refused_code(runtime.reuse(&a.id(), &uses("box", "b"))),
            "engine.peer_cycle"
        );
        let _c = runtime
            .create(config("other").named("c").using("box", "b"))
            .unwrap();
        // c uses b uses a: a may not use c either.
        assert_eq!(
            refused_code(runtime.reuse(&a.id(), &uses("box", "c"))),
            "engine.peer_cycle"
        );
    }
}
