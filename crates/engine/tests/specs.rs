//! What an action takes and gives back: checked on the way in by the
//! runtime, on the way out by a provider's tests.

mod support;

use beatbox_engine::{
    ActionSpec, Field, Kind, MockSandbox, ProviderSpec, Recorder, Sandbox, SandboxRegistry, data,
};
use support::{build, config};

fn execute_spec() -> ActionSpec {
    ActionSpec::new(
        "process.execute",
        vec![
            Field::required("command", Kind::Text),
            Field::optional("linger", Kind::Bool),
        ],
        vec![
            Field::required("pid", Kind::Number),
            Field::required("command", Kind::Text),
            Field::optional("exit_code", Kind::Number),
        ],
    )
}

fn running() -> (Recorder, Sandbox) {
    let (runtime, log) = build(
        vec![MockSandbox::spec().with_actions(&[execute_spec()])],
        None,
    );
    let sandbox = runtime.create(config("mock")).unwrap();
    sandbox.start().unwrap();
    log.clear();
    (log, sandbox)
}

#[test]
fn a_missing_argument_is_refused_before_the_provider_is_asked() {
    let (log, sandbox) = running();
    let outcome = sandbox.act("process.execute", data!({})).unwrap();
    let why = outcome.reason.unwrap();
    assert_eq!(why.code, "engine.missing_argument");
    assert_eq!(why.values["argument"], "command");
    // Refused, not attempted: an attempt is recorded as `sandbox.action`.
    assert_eq!(log.names(), ["sandbox.refused"]);
}

#[test]
fn a_null_is_a_value_left_out() {
    let (_log, sandbox) = running();
    let outcome = sandbox
        .act("process.execute", data!({"command": null}))
        .unwrap();
    assert_eq!(outcome.reason.unwrap().code, "engine.missing_argument");
}

#[test]
fn an_argument_of_the_wrong_kind_is_refused_by_name() {
    let (_log, sandbox) = running();
    let outcome = sandbox
        .act(
            "process.execute",
            data!({"command": "/bin/true", "linger": "yes"}),
        )
        .unwrap();
    let why = outcome.reason.unwrap();
    assert_eq!(why.code, "engine.bad_argument");
    assert_eq!(why.values["argument"], "linger");
    assert_eq!(why.values["kind"], "bool");
}

#[test]
fn an_argument_nobody_declared_passes() {
    let (_log, sandbox) = running();
    let outcome = sandbox
        .act(
            "process.execute",
            data!({"command": "/bin/true", "exit_code": 3}),
        )
        .unwrap();
    assert!(outcome.ok);
    assert_eq!(outcome.data["exit_code"], 3);
    execute_spec().assert_outputs(&outcome.data);
}

#[test]
fn a_capability_with_no_spec_is_not_checked() {
    let (_log, sandbox) = running();
    let outcome = sandbox
        .act("filesystem.write", data!({"path": "/a", "content": "b"}))
        .unwrap();
    assert!(outcome.ok);
    let outcome = sandbox.act("filesystem.read", data!({})).unwrap();
    // Refused by the provider, as before: the runtime had nothing to check.
    assert_eq!(outcome.reason.unwrap().code, "engine.missing_argument");
}

#[test]
#[should_panic(expected = "process.execute answered with pid missing")]
fn an_answer_that_breaks_its_spec_fails_the_test() {
    execute_spec().assert_outputs(&data!({"command": "/bin/true"}));
}

#[test]
fn a_spec_for_a_capability_the_type_does_not_declare_is_a_broken_contract() {
    let mut registry = SandboxRegistry::default();
    let wrong =
        MockSandbox::spec().with_actions(&[ActionSpec::new("network.attach", vec![], vec![])]);
    assert!(registry.register(wrong).is_err());
}

#[test]
fn describing_a_capability_twice_is_a_broken_contract() {
    let mut registry = SandboxRegistry::default();
    let twice: ProviderSpec = MockSandbox::spec().with_actions(&[execute_spec(), execute_spec()]);
    assert!(registry.register(twice).is_err());
}
