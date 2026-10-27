//! What can be built here, and finding it by capability instead of name.
//!
//! Two cases live as `compile_fail` doctests instead: a non-provider can't
//! register ([`SandboxRegistry`]), and a provider must implement the floor
//! (`runtime::provider`).

mod support;

use beatbox_engine::{EngineError, Isolation, MockSandbox, SandboxRegistry};
use support::{Hooks, build, config, plain, plain_with};

mod registration_tests {
    use super::*;

    #[test]
    fn a_type_publishes_what_its_class_declared() {
        let mut registry = SandboxRegistry::default();
        let entry = registry.register(MockSandbox::spec()).unwrap();
        assert_eq!(entry.name, "mock");
        assert_eq!(entry.version, "1");
        assert_eq!(entry.isolation, Isolation::None);
        let mut declared: Vec<String> = beatbox_engine::runtime::providers::MOCK_CAPABILITIES
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
        declared.sort();
        assert_eq!(entry.capabilities.to_list(), declared);
        assert_eq!(entry.spec().name, "mock");
    }

    /// Taking the second silently is how a test double ends up in production.
    #[test]
    fn registering_the_same_name_twice_is_refused() {
        let mut registry = SandboxRegistry::default();
        registry.register(MockSandbox::spec()).unwrap();
        assert!(registry.register(MockSandbox::spec()).is_err());
    }

    #[test]
    fn replacing_is_possible_when_it_is_asked_for() {
        let mut registry = SandboxRegistry::default();
        registry.register(MockSandbox::spec()).unwrap();
        assert_eq!(
            registry
                .register_replacing(MockSandbox::spec())
                .unwrap()
                .name,
            "mock"
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_type_name_is_one_lowercase_word() {
        let mut registry = SandboxRegistry::default();
        assert!(
            registry
                .register(plain().with_name("Host Sandbox"))
                .is_err()
        );
    }

    /// Checked at registration, the last point before the promise goes out.
    #[test]
    fn a_misspelled_capability_is_caught_when_the_class_is_written() {
        let mut registry = SandboxRegistry::default();
        let wrong = plain_with(
            "wrong",
            &["Process.Execute"],
            Isolation::None,
            Hooks::default(),
        );
        assert!(registry.register(wrong).is_err());
    }

    #[test]
    fn an_isolation_level_nobody_can_rank_is_caught_the_same_way() {
        assert!(Isolation::parse("strict").is_err());
        assert_eq!(Isolation::parse("machine").unwrap(), Isolation::Machine);
    }
}

mod discovery_tests {
    use super::*;

    fn registry() -> SandboxRegistry {
        let mut registry = SandboxRegistry::default();
        registry.register(MockSandbox::spec()).unwrap();
        registry.register(plain()).unwrap();
        registry
    }

    #[test]
    fn what_types_are_there() {
        let registry = registry();
        assert_eq!(registry.names(), ["mock", "plain"]);
        assert!(registry.contains("mock"));
    }

    #[test]
    fn who_can_run_a_process_for_me() {
        let registry = registry();
        let found: Vec<String> = registry
            .providing("process.execute")
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(found, ["mock"]);
        assert!(registry.providing("traffic.capture").is_empty());
    }

    #[test]
    fn a_type_can_be_browsed_by_namespace() {
        let entry = registry().get("mock").unwrap();
        assert_eq!(
            entry.capabilities.matching("filesystem.*").unwrap(),
            ["filesystem.list", "filesystem.read", "filesystem.write"]
        );
        assert!(
            entry
                .capabilities
                .namespaces()
                .contains(&"snapshot".to_owned())
        );
    }

    /// So a bridge can send it to an interface.
    #[test]
    fn a_type_describes_itself_as_data() {
        let described = registry().get("plain").unwrap().to_value();
        assert_eq!(
            described,
            serde_json::json!({"name": "plain", "version": "1", "capabilities": [], "isolation": "none"})
        );
    }
}

mod unknown_type_tests {
    use super::*;

    /// Not a bug: a record from a build that had the type, opened on one
    /// that doesn't.
    #[test]
    fn asking_for_a_type_that_is_not_here_is_an_ordinary_refusal() {
        let (runtime, _log) = build(vec![], None);
        let Err(EngineError::Refused(why)) = runtime.create(config("host")) else {
            panic!("a type nobody registered was built");
        };
        assert_eq!(why.code, "engine.unknown_type");
        assert_eq!(why.values["type"], "host");
        assert_eq!(why.values.len(), 1);
    }

    /// There is no global one, and this is why that matters.
    #[test]
    fn two_runtimes_do_not_share_a_registry() {
        let (first, _) = build(vec![MockSandbox::spec()], None);
        let (second, _) = build(vec![plain()], None);
        assert_eq!(first.registry().names(), ["mock"]);
        assert_eq!(second.registry().names(), ["plain"]);
    }
}
