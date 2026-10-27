//! Amounts with units, and a ledger that adds up.
//!
//! Honest book-keeping, refusals a person can read, and resources given back
//! on destroy, not on failure.

mod support;

use beatbox_engine::runtime::resources::{CPU, MEMORY, STORAGE, describe};
use beatbox_engine::{EngineError, Lifecycle, MockSandbox, NOTHING, ResourceLedger, Resources};
use serde_json::json;
use support::{build, config};

fn amounts(pairs: &[(beatbox_engine::ResourceKind, f64)]) -> Resources {
    Resources::new(pairs.iter().copied()).unwrap()
}

mod amount_tests {
    use super::*;

    #[test]
    fn asking_for_none_and_not_asking_are_the_same_request() {
        assert_eq!(amounts(&[(CPU, 0.0)]), Resources::default());
        assert!(amounts(&[(CPU, 0.0)]).is_empty());
    }

    #[test]
    fn a_kind_nobody_declared_is_a_bug() {
        assert!(Resources::from_value(&json!({"gpu_hours": 3})).is_err());
    }

    #[test]
    fn a_negative_amount_is_a_bug() {
        assert!(Resources::new([(CPU, -1.0)]).is_err());
    }

    #[test]
    fn arithmetic_never_goes_below_zero() {
        assert_eq!(
            amounts(&[(CPU, 2.0)]).minus(&amounts(&[(CPU, 5.0)])),
            NOTHING
        );
        assert_eq!(
            serde_json::Value::Object(
                amounts(&[(CPU, 2.0)])
                    .plus(&amounts(&[(CPU, 2.0), (MEMORY, 512.0)]))
                    .to_data()
            ),
            json!({"cpu": 4.0, "memory": 512.0})
        );
    }

    #[test]
    fn kinds_come_back_in_the_order_a_reader_looks_at_them() {
        let asked = amounts(&[(STORAGE, 40960.0), (CPU, 4.0), (MEMORY, 8192.0)]);
        assert_eq!(asked.kinds(), [CPU, MEMORY, STORAGE]);
    }

    #[test]
    fn one_of_something_reads_as_one() {
        assert_eq!(describe(CPU, 1.0), "1 core");
        assert_eq!(describe(CPU, 4.0), "4 cores");
        assert_eq!(describe(MEMORY, 8192.0), "8192 MiB");
    }

    #[test]
    fn a_request_survives_being_written_down_and_read_back() {
        let asked = amounts(&[(CPU, 4.0), (MEMORY, 8192.0)]);
        let written = serde_json::Value::Object(asked.to_data());
        assert_eq!(Resources::from_value(&written).unwrap(), asked);
    }
}

mod ledger_tests {
    use super::*;

    #[test]
    fn an_unbounded_ledger_refuses_nothing_and_still_counts() {
        let mut ledger = ResourceLedger::default();
        ledger.allocate("sbx-1", &amounts(&[(CPU, 64.0)])).unwrap();
        assert_eq!(ledger.committed().get(CPU), 64.0);
        assert!(
            ledger.available().is_none(),
            "there is no budget to be left of"
        );
    }

    #[test]
    fn what_is_left_is_what_was_not_promised() {
        let mut ledger = ResourceLedger::with_budget(amounts(&[(CPU, 8.0), (MEMORY, 16384.0)]));
        ledger
            .allocate("sbx-1", &amounts(&[(CPU, 6.0), (MEMORY, 8192.0)]))
            .unwrap();
        assert_eq!(
            ledger.available().unwrap(),
            amounts(&[(CPU, 2.0), (MEMORY, 8192.0)])
        );
    }

    #[test]
    fn a_refusal_says_which_resource_ran_out_in_words() {
        let mut ledger = ResourceLedger::with_budget(amounts(&[(CPU, 4.0)]));
        ledger.allocate("sbx-1", &amounts(&[(CPU, 3.0)])).unwrap();
        let Err(EngineError::Refused(why)) = ledger.allocate("sbx-2", &amounts(&[(CPU, 2.0)]))
        else {
            panic!("an allocation the budget cannot cover was made");
        };
        assert_eq!(why.code, "engine.over_budget");
        assert_eq!(why.values["resource"], "cores");
        assert_eq!(why.values["requested"], "2 cores");
        assert_eq!(why.values["available"], "1 core");
        assert_eq!(why.values.len(), 3);
    }

    #[test]
    fn a_refused_allocation_promises_nothing_at_all() {
        let mut ledger = ResourceLedger::with_budget(amounts(&[(CPU, 1.0)]));
        assert!(
            ledger
                .allocate("sbx-1", &amounts(&[(CPU, 2.0), (MEMORY, 512.0)]))
                .is_err()
        );
        assert!(ledger.holders().is_empty());
        assert_eq!(ledger.committed(), NOTHING);
    }

    /// Stable, rather than whichever the map happened to yield.
    #[test]
    fn the_first_kind_that_does_not_fit_is_the_one_named() {
        let mut ledger = ResourceLedger::with_budget(amounts(&[(CPU, 1.0), (MEMORY, 128.0)]));
        let Err(EngineError::Refused(why)) =
            ledger.allocate("sbx-1", &amounts(&[(MEMORY, 512.0), (CPU, 8.0)]))
        else {
            panic!("an allocation the budget cannot cover was made");
        };
        assert_eq!(why.values["resource"], "cores");
    }

    #[test]
    fn releasing_twice_is_not_an_error() {
        let mut ledger = ResourceLedger::default();
        ledger.allocate("sbx-1", &amounts(&[(CPU, 2.0)])).unwrap();
        assert_eq!(ledger.release("sbx-1").get(CPU), 2.0);
        assert_eq!(ledger.release("sbx-1"), NOTHING);
    }

    #[test]
    fn allocating_twice_for_one_sandbox_is_a_bug() {
        let mut ledger = ResourceLedger::default();
        ledger.allocate("sbx-1", &amounts(&[(CPU, 2.0)])).unwrap();
        assert!(matches!(
            ledger.allocate("sbx-1", &amounts(&[(CPU, 2.0)])),
            Err(EngineError::Contract(_))
        ));
    }
}

mod runtime_ledger_tests {
    use super::*;

    fn four_cores() -> Option<ResourceLedger> {
        Some(ResourceLedger::with_budget(amounts(&[(CPU, 4.0)])))
    }

    #[test]
    fn a_runtime_with_a_budget_refuses_the_sandbox_it_cannot_honour() {
        let (runtime, _) = build(vec![], four_cores());
        runtime
            .create(config("mock").with_resources(amounts(&[(CPU, 4.0)])))
            .unwrap();
        let Err(EngineError::Refused(why)) =
            runtime.create(config("mock").with_resources(amounts(&[(CPU, 1.0)])))
        else {
            panic!("a sandbox the budget cannot cover was created");
        };
        assert_eq!(why.code, "engine.over_budget");
        assert_eq!(runtime.len(), 1, "and does not half-create it");
    }

    #[test]
    fn destroying_gives_it_back() {
        let (runtime, _) = build(vec![], four_cores());
        let sandbox = runtime
            .create(config("mock").with_resources(amounts(&[(CPU, 4.0)])))
            .unwrap();
        assert_eq!(runtime.resources().available().unwrap().get(CPU), 0.0);
        sandbox.destroy().unwrap();
        assert_eq!(runtime.resources().available().unwrap().get(CPU), 4.0);
    }

    /// A guest that died still has a disk. It lets go when it is destroyed,
    /// which is a decision somebody makes.
    #[test]
    fn a_failed_sandbox_keeps_holding_what_it_was_given() {
        let (runtime, _) = build(vec![], four_cores());
        let sandbox = runtime
            .create(config("mock").with_resources(amounts(&[(CPU, 4.0)])))
            .unwrap();
        sandbox.start().unwrap();
        sandbox.provider(|p: &mut MockSandbox| p.crash()).unwrap();
        sandbox.refresh().unwrap();

        assert_eq!(sandbox.lifecycle(), Lifecycle::Failed);
        assert_eq!(runtime.resources().available().unwrap().get(CPU), 0.0);
        sandbox.destroy().unwrap();
        assert_eq!(runtime.resources().available().unwrap().get(CPU), 4.0);
    }
}
