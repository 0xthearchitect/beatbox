//! The state machine, and the three gates in front of it.
//!
//! The interesting asserts are the refusals: starting twice, pausing without
//! the capability, acting after death.

mod support;

use beatbox_engine::runtime::lifecycle::{
    self, CALLABLE, LIFECYCLES, TRANSITIONS, next_state, transitions_from,
};
use beatbox_engine::{EngineError, Lifecycle, MockSandbox, Transition, data};
use support::{Plain, build, config, mock, plain};

use Lifecycle::*;
use Transition as T;

mod table_tests {
    use super::*;

    #[test]
    fn every_state_offers_exactly_what_the_plan_drew() {
        assert_eq!(transitions_from(Created), [T::Start, T::Destroy]);
        assert_eq!(
            transitions_from(Running),
            [T::Stop, T::Pause, T::Reset, T::Destroy]
        );
        assert_eq!(
            transitions_from(Paused),
            [T::Stop, T::Resume, T::Reset, T::Destroy]
        );
        assert_eq!(transitions_from(Stopped), [T::Start, T::Reset, T::Destroy]);
        assert_eq!(transitions_from(Failed), [T::Reset, T::Destroy]);
    }

    #[test]
    fn destroyed_is_the_end() {
        assert_eq!(transitions_from(Destroyed), []);
        for transition in TRANSITIONS {
            assert_eq!(next_state(Destroyed, transition), None, "{transition}");
        }
    }

    #[test]
    fn everything_can_be_destroyed() {
        for state in LIFECYCLES {
            if state == Destroyed {
                continue;
            }
            assert_eq!(next_state(state, T::Destroy), Some(Destroyed), "{state}");
        }
    }

    /// It is a legal edge and it is not on offer: nobody gets to pretend
    /// something broke.
    #[test]
    fn failing_is_not_something_a_caller_can_ask_for() {
        assert!(TRANSITIONS.contains(&T::Fail));
        assert!(!CALLABLE.contains(&T::Fail));
        for state in LIFECYCLES.into_iter().filter(|s| s.is_live()) {
            assert!(!transitions_from(state).contains(&T::Fail));
        }
    }

    /// Whatever died has to be reset or destroyed first.
    #[test]
    fn a_failed_sandbox_cannot_simply_be_started_again() {
        assert_eq!(next_state(Failed, T::Start), None);
        assert_eq!(next_state(Failed, T::Reset), Some(Created));
    }
}

mod transition_tests {
    use super::*;

    fn set_up() -> (
        beatbox_engine::SandboxRuntime,
        beatbox_engine::Recorder,
        beatbox_engine::Sandbox,
    ) {
        let (runtime, log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        (runtime, log, sandbox)
    }

    #[test]
    fn the_whole_round_trip() {
        let (_runtime, _log, sandbox) = set_up();
        assert_eq!(sandbox.lifecycle(), Created);
        assert_eq!(sandbox.start().unwrap().lifecycle, Running);
        assert_eq!(sandbox.pause().unwrap().lifecycle, Paused);
        assert_eq!(sandbox.resume().unwrap().lifecycle, Running);
        assert_eq!(sandbox.stop().unwrap().lifecycle, Stopped);
        assert_eq!(sandbox.start().unwrap().lifecycle, Running);
        assert_eq!(sandbox.reset().unwrap().lifecycle, Created);
        assert_eq!(sandbox.destroy().unwrap().lifecycle, Destroyed);
    }

    #[test]
    fn a_refused_transition_names_where_it_was_asked_from() {
        let (_runtime, _log, sandbox) = set_up();
        let Err(EngineError::Refused(why)) = sandbox.pause() else {
            panic!("pausing something created was not refused");
        };
        assert_eq!(why.code, "engine.bad_transition");
        assert_eq!(why.values["state"], "created");
        assert_eq!(why.values["transition"], "pause");
        assert_eq!(why.values.len(), 2);
    }

    /// What makes a refusal safe to catch: nothing was attempted, so there
    /// is nothing to undo before trying something else.
    #[test]
    fn a_refusal_changes_nothing_at_all() {
        let (_runtime, _log, sandbox) = set_up();
        let before = sandbox.state();
        assert!(matches!(sandbox.pause(), Err(EngineError::Refused(_))));
        assert_eq!(sandbox.state(), before);
    }

    #[test]
    fn a_destroyed_sandbox_refuses_everything() {
        let (_runtime, _log, sandbox) = set_up();
        sandbox.destroy().unwrap();
        for attempt in [T::Start, T::Stop, T::Reset, T::Destroy] {
            assert!(
                matches!(sandbox.apply(attempt), Err(EngineError::Refused(_))),
                "{attempt}"
            );
        }
    }

    /// One sandbox that was reset, not two that look alike -- which is what a
    /// recording has to be able to say.
    #[test]
    fn reset_keeps_the_sandbox_it_resets() {
        let (runtime, _log, sandbox) = set_up();
        let identity = sandbox.identity();
        sandbox.start().unwrap();
        sandbox.reset().unwrap();
        assert_eq!(sandbox.identity(), identity);
        assert!(runtime.get(&identity.id).unwrap().same(&sandbox));
    }

    #[test]
    fn every_transition_is_announced() {
        let (_runtime, log, sandbox) = set_up();
        sandbox.start().unwrap();
        sandbox.pause().unwrap();
        sandbox.resume().unwrap();
        sandbox.stop().unwrap();
        sandbox.destroy().unwrap();
        let said: Vec<String> = log
            .names()
            .into_iter()
            .filter(|name| name.starts_with("sandbox."))
            .collect();
        assert_eq!(
            said,
            [
                "sandbox.created",
                "sandbox.started",
                "sandbox.paused",
                "sandbox.resumed",
                "sandbox.stopped",
                "sandbox.destroyed",
            ]
        );
    }

    /// Asking for something a sandbox can't do is a caller's bug, and the
    /// record is where somebody finds it.
    #[test]
    fn a_refusal_is_announced_too() {
        let (_runtime, log, sandbox) = set_up();
        assert!(sandbox.pause().is_err());
        let refused = log.matching("sandbox.refused");
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].get("reason").unwrap(), "engine.bad_transition");
    }
}

/// Optional verbs are refused by the runtime, not by the provider.
mod capability_gate_tests {
    use super::*;

    #[test]
    fn a_sandbox_that_cannot_pause_is_not_asked_to() {
        let (runtime, _log) = build(vec![plain()], None);
        let sandbox = runtime.create(config("plain")).unwrap();
        sandbox.start().unwrap();
        let Err(EngineError::Refused(why)) = sandbox.pause() else {
            panic!("a sandbox that cannot pause was paused");
        };
        assert_eq!(why.code, "engine.unsupported");
        assert_eq!(why.values["capability"], "lifecycle.pause");
        let calls = sandbox.provider(|p: &mut Plain| p.calls.clone()).unwrap();
        assert!(!calls.contains(&"pause".to_owned()));
    }

    #[test]
    fn the_floor_is_always_available() {
        let (runtime, _log) = build(vec![plain()], None);
        let sandbox = runtime.create(config("plain")).unwrap();
        sandbox.start().unwrap();
        sandbox.stop().unwrap();
        sandbox.destroy().unwrap();
        let calls = sandbox.provider(|p: &mut Plain| p.calls.clone()).unwrap();
        assert_eq!(calls, ["create", "start", "stop", "destroy"]);
    }
}

mod failure_tests {
    use super::*;

    #[test]
    fn a_provider_that_breaks_without_losing_the_sandbox_leaves_it_where_it_was() {
        let (runtime, log) = mock();
        let sandbox = runtime
            .create(config("mock").with_settings(data!({"refuse": ["start"]})))
            .unwrap();
        let Err(EngineError::Provider(broken)) = sandbox.start() else {
            panic!("a start the mock was told to refuse succeeded");
        };
        assert_eq!(broken.reason.unwrap().code, "engine.mock_refused");
        assert!(!broken.fatal);
        assert_eq!(sandbox.lifecycle(), Created);

        let broke = log.matching("sandbox.broke");
        assert_eq!(
            broke.len(),
            1,
            "an attempt that produced nothing would read as one nobody made"
        );
        assert_eq!(broke[0].get("transition").unwrap(), "start");
        assert_eq!(broke[0].get("reason").unwrap(), "engine.mock_refused");
    }

    /// `refused` never tried, `broke` tried and survived, `failed` tried and
    /// did not. A recording has to be readable without guessing.
    #[test]
    fn the_three_ways_something_does_not_happen_have_three_names() {
        let (runtime, log) = mock();
        let sandbox = runtime
            .create(config("mock").with_settings(data!({"refuse": ["pause"]})))
            .unwrap();
        assert!(matches!(sandbox.stop(), Err(EngineError::Refused(_)))); // not running
        sandbox.start().unwrap();
        assert!(matches!(sandbox.pause(), Err(EngineError::Provider(_)))); // survived
        sandbox.provider(|p: &mut MockSandbox| p.crash()).unwrap();
        sandbox.refresh().unwrap(); // gone
        let said: Vec<String> = log
            .names()
            .into_iter()
            .filter(|n| {
                ["sandbox.refused", "sandbox.broke", "sandbox.failed"].contains(&n.as_str())
            })
            .collect();
        assert_eq!(said, ["sandbox.refused", "sandbox.broke", "sandbox.failed"]);
    }

    #[test]
    fn a_provider_that_loses_the_sandbox_moves_it_to_failed() {
        let (runtime, log) = mock();
        let sandbox = runtime
            .create(config("mock").with_settings(data!({"refuse": ["start"], "fatal": true})))
            .unwrap();
        assert!(matches!(sandbox.start(), Err(EngineError::Provider(_))));
        assert_eq!(sandbox.lifecycle(), Failed);
        assert_eq!(sandbox.state().reason.unwrap().code, "engine.mock_refused");
        assert!(log.names().contains(&"sandbox.failed".to_owned()));
    }

    /// Why `observe` exists: nothing failed, it just stopped being there.
    #[test]
    fn a_sandbox_the_world_took_away_is_found_by_asking() {
        let (runtime, log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        sandbox.start().unwrap();
        sandbox.provider(|p: &mut MockSandbox| p.crash()).unwrap();

        assert_eq!(sandbox.lifecycle(), Running, "nothing has looked yet");
        assert_eq!(sandbox.refresh().unwrap().lifecycle, Failed);
        assert_eq!(sandbox.state().reason.unwrap().code, "engine.sandbox_lost");
        assert_eq!(log.names().last().unwrap(), "sandbox.failed");
    }

    #[test]
    fn a_dead_sandbox_is_never_quietly_resurrected() {
        let (runtime, _log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        sandbox.start().unwrap();
        sandbox.provider(|p: &mut MockSandbox| p.crash()).unwrap();
        sandbox.refresh().unwrap();

        sandbox.provider(|p: &mut MockSandbox| p.uncrash()).unwrap(); // the world changing its mind
        assert_eq!(sandbox.refresh().unwrap().lifecycle, Failed);
        assert_eq!(
            sandbox.reset().unwrap().lifecycle,
            Created,
            "coming back is a decision, and this is it"
        );
    }

    /// The reason a transition is followed by an observation.
    #[test]
    fn a_guest_that_dies_the_instant_it_starts_does_not_sit_in_running() {
        let (runtime, log) = mock();
        let sandbox = runtime.create(config("mock")).unwrap();
        sandbox.provider(|p: &mut MockSandbox| p.crash()).unwrap();
        sandbox.start().unwrap();
        assert_eq!(sandbox.lifecycle(), Failed);
        let names = log.names();
        assert_eq!(
            names[names.len() - 2..],
            ["sandbox.started", "sandbox.failed"]
        );
    }
}

mod observation_tests {
    use super::*;
    use serde_json::json;

    fn set_up() -> (beatbox_engine::SandboxRuntime, beatbox_engine::Sandbox) {
        let (runtime, _log) = mock();
        let sandbox = runtime
            .create(config("mock").with_settings(data!({"files": {"/etc/hosts": "127.0.0.1"}})))
            .unwrap();
        (runtime, sandbox)
    }

    #[test]
    fn a_state_handed_out_has_always_been_observed() {
        let (_runtime, sandbox) = set_up();
        assert_eq!(sandbox.state().facts["files"], 1);
        assert_eq!(sandbox.start().unwrap().facts["processes"], json!(["init"]));
    }

    /// Old addresses on a restarted sandbox would mislead an investigation.
    #[test]
    fn a_transition_does_not_carry_the_old_facts_across() {
        let (_runtime, sandbox) = set_up();
        sandbox.start().unwrap();
        assert_eq!(sandbox.state().facts["processes"], json!(["init"]));
        sandbox.stop().unwrap();
        assert_eq!(sandbox.state().facts["processes"], json!([]));
    }

    #[test]
    fn the_moment_of_the_last_change_is_recorded() {
        let (_runtime, sandbox) = set_up();
        let first = sandbox.state().since;
        sandbox.start().unwrap();
        assert_ne!(sandbox.state().since, first);
        let observed = sandbox.refresh().unwrap();
        assert_eq!(
            observed.since,
            sandbox.state().since,
            "observing is not a change"
        );
    }
}

#[test]
fn the_lifecycle_words_are_the_ones_on_the_wire() {
    for state in LIFECYCLES {
        assert_eq!(state.as_str().parse::<Lifecycle>().unwrap(), state);
    }
    for transition in TRANSITIONS {
        assert_eq!(
            transition.as_str().parse::<Transition>().unwrap(),
            transition
        );
    }
    assert!(lifecycle::allows(Running, T::Pause));
}
