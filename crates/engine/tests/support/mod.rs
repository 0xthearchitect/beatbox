//! A runtime with a boring clock, for tests that assert on what happened.
//!
//! A runtime wired to a recorder, and time that doesn't move on its own.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use beatbox_engine::clock::{Clock, Ids};
use beatbox_engine::{
    Action, CapabilitySet, Data, Emitter, EventBus, Isolation, MockSandbox, Observation,
    ProviderError, ProviderSpec, Recorder, ResourceLedger, RuntimeParts, SandboxConfig,
    SandboxIdentity, SandboxProvider, SandboxRegistry, SandboxRuntime,
};

/// `2026-01-01T00:00:01+00:00`, then one second later, then another.
pub fn ticking_clock() -> Clock {
    let moment = Arc::new(Mutex::new(0u32));
    Arc::new(move || {
        let mut moment = moment.lock().unwrap();
        *moment += 1;
        format!("2026-01-01T00:00:{:02}+00:00", *moment)
    })
}

/// `sbx-1`, `sbx-2`, ...
pub fn counting_ids() -> Ids {
    let counter = Arc::new(Mutex::new(0u32));
    Arc::new(move || {
        let mut counter = counter.lock().unwrap();
        *counter += 1;
        format!("sbx-{}", *counter)
    })
}

/// A runtime with the given types registered (the mock, if none), and
/// everything recorded.
pub fn build(
    specs: Vec<ProviderSpec>,
    ledger: Option<ResourceLedger>,
) -> (SandboxRuntime, Recorder) {
    let mut registry = SandboxRegistry::default();
    let specs = if specs.is_empty() {
        vec![MockSandbox::spec()]
    } else {
        specs
    };
    for spec in specs {
        registry.register(spec).unwrap();
    }
    let clock = ticking_clock();
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        events: Some(Arc::new(EventBus::new(Arc::clone(&clock)))),
        ledger: ledger.unwrap_or_default(),
        clock: Some(clock),
        ids: Some(counting_ids()),
    });
    let log = Recorder::default();
    runtime.events().subscribe("*", log.sink()).unwrap();
    (runtime, log)
}

/// The mock alone, with nothing limiting it.
pub fn mock() -> (SandboxRuntime, Recorder) {
    build(vec![], None)
}

/// A configuration for `type_name` and nothing else.
pub fn config(type_name: &str) -> SandboxConfig {
    SandboxConfig::new(type_name).unwrap()
}

/// A lifecycle step a test overrides.
pub type Step = Arc<dyn Fn(&SandboxIdentity) -> Result<(), ProviderError> + Send + Sync>;
/// An action a test overrides.
pub type Perform = Arc<dyn Fn(&Action) -> Result<Data, ProviderError> + Send + Sync>;
/// A narrowing a test overrides.
pub type Narrow = Arc<dyn Fn(&SandboxConfig) -> CapabilitySet + Send + Sync>;
/// An observation a test overrides.
pub type Look = Arc<dyn Fn() -> Observation + Send + Sync>;

/// What a test overrides on [`Plain`].
#[derive(Clone, Default)]
pub struct Hooks {
    pub create: Option<Step>,
    pub destroy: Option<Step>,
    pub perform: Option<Perform>,
    pub capabilities: Option<Narrow>,
    pub observe: Option<Look>,
}

/// The floor and nothing else: no pausing, no resetting, no actions.
///
/// For checking the runtime's gates refuse before a provider is reached.
pub struct Plain {
    pub calls: Vec<String>,
    pub alive: bool,
    identity: SandboxIdentity,
    config: SandboxConfig,
    hooks: Hooks,
    _emit: Emitter,
}

/// `plain`, version 1, no capabilities, no isolation.
pub fn plain() -> ProviderSpec {
    plain_with("plain", &[], Isolation::None, Hooks::default())
}

/// A variant of [`Plain`]: another name, other capabilities, a boundary,
/// and whichever methods the test overrides.
pub fn plain_with(
    name: &str,
    capabilities: &[&str],
    isolation: Isolation,
    hooks: Hooks,
) -> ProviderSpec {
    ProviderSpec::new(
        name,
        "1",
        capabilities,
        isolation,
        move |identity, config, emit| {
            Box::new(Plain {
                calls: Vec::new(),
                alive: true,
                identity,
                config,
                hooks: hooks.clone(),
                _emit: emit,
            })
        },
    )
}

impl SandboxProvider for Plain {
    fn capabilities(&self) -> Option<CapabilitySet> {
        self.hooks
            .capabilities
            .as_ref()
            .map(|narrow| narrow(&self.config))
    }

    fn create(&mut self) -> Result<(), ProviderError> {
        self.calls.push("create".into());
        match &self.hooks.create {
            Some(create) => create(&self.identity),
            None => Ok(()),
        }
    }

    fn start(&mut self) -> Result<(), ProviderError> {
        self.calls.push("start".into());
        Ok(())
    }

    fn stop(&mut self) -> Result<(), ProviderError> {
        self.calls.push("stop".into());
        Ok(())
    }

    fn destroy(&mut self) -> Result<(), ProviderError> {
        self.calls.push("destroy".into());
        match &self.hooks.destroy {
            Some(destroy) => destroy(&self.identity),
            None => Ok(()),
        }
    }

    fn observe(&mut self) -> Observation {
        if let Some(observe) = &self.hooks.observe {
            return observe();
        }
        Observation {
            alive: Some(self.alive),
            facts: beatbox_engine::data!({"calls": self.calls}),
            ..Observation::default()
        }
    }

    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        match &self.hooks.perform {
            Some(perform) => perform(action),
            None => Err(beatbox_engine::runtime::provider::unsupported(
                action.capability(),
            )),
        }
    }
}
