//! The contract between the runtime and whatever actually runs a sandbox.
//!
//! Lifecycle, capabilities, events and the ledger are written against this
//! trait and nothing else.
//!
//! **One provider per sandbox**, or every provider would keep its own map
//! keyed by sandbox id, which is the runtime's job.
//!
//! `create`, `start`, `stop`, `destroy` and `observe` are required. The rest
//! refuse by default: the runtime already checks capabilities first, and
//! this is the second lock. Leaving one of the five out doesn't compile:
//!
//! ```compile_fail
//! use beatbox_engine::{Observation, SandboxProvider, ProviderError};
//! struct Lazy;
//! impl SandboxProvider for Lazy {
//!     fn observe(&mut self) -> Observation { Observation::default() }
//! }
//! ```
//!
//! A provider that can't do something returns a [`ProviderError`] with a
//! reason, `fatal` if the sandbox didn't survive. A sandbox that's gone
//! isn't an [`observe`] failure: it's `alive: Some(false)`, an answer. A
//! panic is a bug, reported as `engine.provider_failed` instead of taking
//! the process down.
//!
//! [`observe`]: SandboxProvider::observe

use std::any::Any;
use std::sync::Arc;

use beatbox_reasons::reason;

use crate::events::{Data, Event, EventBus};
use crate::faults::{ContractError, ProviderError};
use crate::names::{RESERVED, namespace, validate};
use crate::runtime::actions::Action;
use crate::runtime::capabilities::{
    CapabilitySet, LIFECYCLE_PAUSE, LIFECYCLE_RECONFIGURE, LIFECYCLE_RESET,
};
use crate::runtime::config::SandboxConfig;
use crate::runtime::identity::SandboxIdentity;
use crate::runtime::isolation::Isolation;
use crate::runtime::peers::{Peers, Requirement};
use crate::runtime::specs::ActionSpec;
use crate::runtime::state::Observation;

/// One sandbox's way onto the bus, with its id already filled in.
///
/// A provider's refuses the `sandbox.` namespace; only the sandbox's own
/// emitter may write there.
#[derive(Clone)]
pub struct Emitter {
    sandbox_id: String,
    bus: Arc<EventBus>,
    reserved_ok: bool,
}

impl Emitter {
    pub(crate) fn new(sandbox_id: &str, bus: Arc<EventBus>, reserved_ok: bool) -> Self {
        Self {
            sandbox_id: sandbox_id.to_owned(),
            bus,
            reserved_ok,
        }
    }

    /// An emitter for a provider built outside a runtime (a test, a tool).
    /// It still can't write `sandbox.*`.
    pub fn standalone(sandbox_id: &str, bus: Arc<EventBus>) -> Self {
        Self::new(sandbox_id, bus, false)
    }

    /// Say that something happened, without knowing who is listening.
    ///
    /// The only way out of a provider. `sandbox.*` is refused unless this is
    /// the runtime's own emitter.
    pub fn emit(&self, name: &str, data: Data) -> Result<Event, ContractError> {
        validate(name, "an event name")?;
        if !self.reserved_ok && namespace(name) == RESERVED {
            return Err(ContractError::broken(format!(
                "'{RESERVED}.*' belongs to the runtime, not to a provider: {name}"
            )));
        }
        self.bus.publish(name, &self.sandbox_id, data)
    }

    /// Whose events these are.
    pub fn sandbox_id(&self) -> &str {
        &self.sandbox_id
    }
}

/// Implement this and a sandbox type exists. Nothing else is required.
pub trait SandboxProvider: Any + Send {
    /// What *this* sandbox can do, given how it was configured, or `None`
    /// for everything its type declares.
    ///
    /// Override to narrow: a host with no disk can't snapshot, better said
    /// here than refused later. Anything the type didn't declare is dropped.
    fn capabilities(&self) -> Option<CapabilitySet> {
        None
    }

    /// The sandboxes this one uses, by role. Called once, before `create` or
    /// before the first `observe` after a restart. Types with no roles can
    /// skip it.
    fn given(&mut self, peers: Peers) {
        let _ = peers;
    }

    /// Make whatever has to exist before starting: a disk, a network, a
    /// directory. Afterwards the sandbox is `created`, ready and idle.
    fn create(&mut self) -> Result<(), ProviderError>;

    /// Begin executing.
    fn start(&mut self) -> Result<(), ProviderError>;

    /// Stop executing, keeping whatever `create` made.
    fn stop(&mut self) -> Result<(), ProviderError>;

    /// Take it all away. Called once, from any state, including half way
    /// through a failed `create`: skip that case and disks leak.
    fn destroy(&mut self) -> Result<(), ProviderError>;

    /// What is true right now, as far as this provider can tell.
    ///
    /// Called after every transition and on every fresh read, so keep it
    /// cheap. Gone is `alive: Some(false)`, not a failure.
    fn observe(&mut self) -> Observation;

    /// Freeze. Gated on `lifecycle.pause`.
    fn pause(&mut self) -> Result<(), ProviderError> {
        Err(unsupported(LIFECYCLE_PAUSE))
    }

    /// Thaw. Gated on `lifecycle.pause`, the same one.
    fn resume(&mut self) -> Result<(), ProviderError> {
        Err(unsupported(LIFECYCLE_PAUSE))
    }

    /// Back to how it was created, without being destroyed first. Gated on
    /// `lifecycle.reset`.
    fn reset(&mut self) -> Result<(), ProviderError> {
        Err(unsupported(LIFECYCLE_RESET))
    }

    /// Take `config` in place of the one this was built with. Gated on
    /// `lifecycle.reconfigure`, never called while running. Fail without
    /// changing anything for what can't change after creation (a host's
    /// disk); the runtime then keeps the old configuration.
    fn reconfigure(&mut self, config: &SandboxConfig) -> Result<(), ProviderError> {
        let _ = config;
        Err(unsupported(LIFECYCLE_RECONFIGURE))
    }

    /// Do one thing the capability set promised, and report what came of it.
    ///
    /// An error only when it couldn't be attempted. A program that ran and
    /// failed is data with a non-zero `exit_code`.
    fn perform(&mut self, action: &Action) -> Result<Data, ProviderError> {
        Err(unsupported(action.capability()))
    }
}

/// The refusal for a capability this provider never implemented.
pub fn unsupported(capability: &str) -> ProviderError {
    ProviderError::new(reason!("engine.unsupported", capability = capability))
}

/// How a registered type builds one provider for one sandbox.
pub type Build =
    Arc<dyn Fn(SandboxIdentity, SandboxConfig, Emitter) -> Box<dyn SandboxProvider> + Send + Sync>;

/// A kind of sandbox, as its implementation declares it.
///
/// Checked when registered. Be honest about `isolation`: overstating it is
/// how a sample ends up detonated in a shared kernel.
#[derive(Clone)]
pub struct ProviderSpec {
    /// One lowercase word.
    pub name: String,
    /// The implementation's version.
    pub version: String,
    /// Everything this type can ever do. A sandbox may get fewer, never more.
    pub capabilities: Vec<String>,
    /// The boundary it provides.
    pub isolation: Isolation,
    /// What its capabilities take and give back, where it says (`specs.rs`).
    pub actions: Vec<ActionSpec>,
    /// The roles other sandboxes fill for it (`peers.rs`).
    pub requires: Vec<Requirement>,
    build: Build,
}

impl ProviderSpec {
    /// A type called `name`, built by `build`.
    pub fn new(
        name: &str,
        version: &str,
        capabilities: &[&str],
        isolation: Isolation,
        build: impl Fn(SandboxIdentity, SandboxConfig, Emitter) -> Box<dyn SandboxProvider>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
            capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
            isolation,
            actions: Vec::new(),
            requires: Vec::new(),
            build: Arc::new(build),
        }
    }

    /// The same, called something else.
    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
    }

    /// The same, declaring these capabilities instead.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: &[&str]) -> Self {
        self.capabilities = capabilities.iter().map(|c| (*c).to_owned()).collect();
        self
    }

    /// The same, providing this boundary instead.
    #[must_use]
    pub fn with_isolation(mut self, isolation: Isolation) -> Self {
        self.isolation = isolation;
        self
    }

    /// The same, saying what these capabilities take and give back.
    #[must_use]
    pub fn with_actions(mut self, actions: &[ActionSpec]) -> Self {
        self.actions = actions.to_vec();
        self
    }

    /// The same, needing these roles filled.
    #[must_use]
    pub fn with_requires(mut self, requires: &[Requirement]) -> Self {
        self.requires = requires.to_vec();
        self
    }

    /// One provider, for one sandbox.
    pub fn build(
        &self,
        identity: SandboxIdentity,
        config: SandboxConfig,
        emit: Emitter,
    ) -> Box<dyn SandboxProvider> {
        (self.build)(identity, config, emit)
    }
}

impl std::fmt::Debug for ProviderSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderSpec")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("capabilities", &self.capabilities)
            .field("isolation", &self.isolation)
            .field("actions", &self.actions)
            .field("requires", &self.requires)
            .finish_non_exhaustive()
    }
}
