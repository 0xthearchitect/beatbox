//! One sandbox: its identity, what was asked for, what is true, and the rules.
//!
//! The provider knows how; this knows whether, and says so.
//!
//! **Gates before the provider is touched:** is the transition legal, was
//! the capability declared. So a refusal never leaves anything half-done.
//! Only the runtime writes the lifecycle; a provider reports and fails.
//!
//! Every transition is followed by a best-effort observation. That's what
//! catches a guest that exits the instant it starts.
//!
//! A [`Sandbox`] is a cheap handle; clones are the same sandbox. Doing
//! something takes the sandbox's own lock (its lane) for the whole
//! operation, so operations on one sandbox never overlap and two sandboxes
//! never wait on each other. Reading copies a view behind a separate lock,
//! so it never waits on a ten-minute save point.

use std::any::Any;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use beatbox_reasons::{Reason, reason};
use serde_json::{Value, json};

use crate::clock::Clock;
use crate::events::Data;
use crate::events::bus::lock;
use crate::faults::{ContractError, EngineError, ProviderError};
use crate::runtime::actions::{Action, ActionOutcome};
use crate::runtime::capabilities::{
    CapabilitySet, LIFECYCLE_PAUSE, LIFECYCLE_RECONFIGURE, LIFECYCLE_RESET,
};
use crate::runtime::config::SandboxConfig;
use crate::runtime::identity::SandboxIdentity;
use crate::runtime::lifecycle::{Lifecycle, Transition, allows, next_state};
use crate::runtime::provider::{Emitter, SandboxProvider};
use crate::runtime::resources::ResourceLedger;
use crate::runtime::specs::ActionSpec;
use crate::runtime::state::SandboxState;

/// One name per transition. `sandbox.created`, `sandbox.action`,
/// `sandbox.reconfigured`, `sandbox.refused` and `sandbox.broke` are written
/// elsewhere. Providers can't emit any of them.
fn event_for(transition: Transition) -> &'static str {
    match transition {
        Transition::Start => "sandbox.started",
        Transition::Stop => "sandbox.stopped",
        Transition::Pause => "sandbox.paused",
        Transition::Resume => "sandbox.resumed",
        Transition::Reset => "sandbox.reset",
        Transition::Destroy => "sandbox.destroyed",
        Transition::Fail => "sandbox.failed",
    }
}

/// Which transitions need a capability first. Start, stop, destroy and fail
/// are the floor every sandbox has.
fn gate(transition: Transition) -> Option<&'static str> {
    match transition {
        Transition::Pause | Transition::Resume => Some(LIFECYCLE_PAUSE),
        Transition::Reset => Some(LIFECYCLE_RESET),
        _ => None,
    }
}

/// Where a configuration may change: nothing executing. A paused guest was
/// sized at start; a failed one needs reset or destroy first.
const RECONFIGURABLE: [Lifecycle; 2] = [Lifecycle::Created, Lifecycle::Stopped];

/// A reason, flattened into event data. Names travel; sentences do not.
pub(crate) fn reason_data(why: Option<&Reason>) -> Data {
    let mut data = Data::new();
    if let Some(why) = why {
        data.insert("reason".into(), Value::String(why.code.clone()));
        data.insert("values".into(), json!(why.values));
    }
    data
}

fn merged(mut data: Data, more: Data) -> Data {
    data.extend(more);
    data
}

/// What a panic said, for the reason that reports it.
pub(crate) fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic with no message".to_owned()
    }
}

/// A provider's bug, named: `engine.provider_failed`.
pub(crate) fn provider_failed(type_name: &str, step: &str, detail: &str) -> Reason {
    reason!(
        "engine.provider_failed",
        type = type_name,
        step = step,
        detail = format!("panic: {detail}"),
    )
}

/// What a sandbox looks like from outside, kept current by every operation.
#[derive(Debug, Clone)]
pub(crate) struct View {
    identity: SandboxIdentity,
    config: SandboxConfig,
    capabilities: CapabilitySet,
    state: SandboxState,
}

fn read(view: &RwLock<View>) -> RwLockReadGuard<'_, View> {
    view.read().unwrap_or_else(PoisonError::into_inner)
}

fn write(view: &RwLock<View>) -> RwLockWriteGuard<'_, View> {
    view.write().unwrap_or_else(PoisonError::into_inner)
}

/// A live sandbox, and the only thing a caller ever holds.
///
/// Built by [`SandboxRuntime`](crate::SandboxRuntime) only after the
/// provider created it, so if you hold one, the sandbox exists. Clones are
/// the same sandbox.
#[derive(Clone)]
pub struct Sandbox {
    core: Arc<Mutex<Core>>,
    view: Arc<RwLock<View>>,
    looks: Arc<Looks>,
}

/// Counts observations so callers queued behind one lane share an answer.
///
/// `begun` is the last one started, `answered` the last that finished
/// without failing. Both move only under the lane; `begun` is read without.
#[derive(Default)]
struct Looks {
    begun: AtomicU64,
    answered: AtomicU64,
}

/// The half that does things, behind the sandbox's own lock: its lane.
struct Core {
    provider: Box<dyn SandboxProvider>,
    actions: Vec<ActionSpec>,
    publish: Emitter,
    ledger: Arc<Mutex<ResourceLedger>>,
    clock: Clock,
    view: Arc<RwLock<View>>,
}

pub(crate) struct Parts {
    pub identity: SandboxIdentity,
    pub config: SandboxConfig,
    pub declared: CapabilitySet,
    pub actions: Vec<ActionSpec>,
    pub provider: Box<dyn SandboxProvider>,
    pub publish: Emitter,
    pub ledger: Arc<Mutex<ResourceLedger>>,
    pub clock: Clock,
    pub state: Option<SandboxState>,
}

impl Sandbox {
    pub(crate) fn new(parts: Parts) -> Self {
        // Through the type's declaration, so a provider can only take powers
        // away from what the registry published.
        let capabilities = match parts.provider.capabilities() {
            Some(narrower) => parts.declared.narrowed_to(&narrower),
            None => parts.declared,
        };
        // Fresh means CREATED. `state` is only for `restore()`, picking up
        // a sandbox at whatever it was last seen as.
        let state = parts
            .state
            .unwrap_or_else(|| SandboxState::new(Lifecycle::Created, (parts.clock)(), None));
        let view = Arc::new(RwLock::new(View {
            identity: parts.identity,
            config: parts.config,
            capabilities,
            state,
        }));
        let core = Core {
            provider: parts.provider,
            actions: parts.actions,
            publish: parts.publish,
            ledger: parts.ledger,
            clock: parts.clock,
            view: Arc::clone(&view),
        };
        Self {
            core: Arc::new(Mutex::new(core)),
            view,
            looks: Arc::default(),
        }
    }

    fn lane(&self) -> std::sync::MutexGuard<'_, Core> {
        lock(&self.core)
    }

    // --- what it is, read from the view: never waits ------------------------

    /// `sbx-3f9c21`.
    pub fn id(&self) -> String {
        read(&self.view).identity.id.clone()
    }

    /// The registered type that built it.
    pub fn type_name(&self) -> String {
        read(&self.view).identity.type_name.clone()
    }

    /// What it is called.
    pub fn name(&self) -> String {
        read(&self.view).identity.name.clone()
    }

    /// Who it is.
    pub fn identity(&self) -> SandboxIdentity {
        read(&self.view).identity.clone()
    }

    /// What was asked for. Never edited by anything here.
    pub fn config(&self) -> SandboxConfig {
        read(&self.view).config.clone()
    }

    /// What it can do.
    pub fn capabilities(&self) -> CapabilitySet {
        read(&self.view).capabilities.clone()
    }

    /// The last thing known, with no I/O. [`Sandbox::refresh`] goes and asks.
    pub fn state(&self) -> SandboxState {
        read(&self.view).state.clone()
    }

    /// Where it is.
    pub fn lifecycle(&self) -> Lifecycle {
        read(&self.view).state.lifecycle
    }

    /// Whether `other` is this same sandbox, rather than one that looks alike.
    pub fn same(&self, other: &Sandbox) -> bool {
        Arc::ptr_eq(&self.core, &other.core)
    }

    /// The provider underneath, if it's a `P`, for tests and its own
    /// tooling. Holds the lane while `use_it` runs.
    pub fn provider<P: SandboxProvider, R>(&self, use_it: impl FnOnce(&mut P) -> R) -> Option<R> {
        let mut core = self.lane();
        let provider: &mut dyn Any = core.provider.as_mut();
        provider.downcast_mut::<P>().map(use_it)
    }

    // --- moving it around -----------------------------------------------------

    /// Begin executing.
    pub fn start(&self) -> Result<SandboxState, EngineError> {
        self.lane().transition(Transition::Start, |p| p.start())
    }

    /// Stop executing.
    pub fn stop(&self) -> Result<SandboxState, EngineError> {
        self.lane().transition(Transition::Stop, |p| p.stop())
    }

    /// Freeze.
    pub fn pause(&self) -> Result<SandboxState, EngineError> {
        self.lane().transition(Transition::Pause, |p| p.pause())
    }

    /// Thaw.
    pub fn resume(&self) -> Result<SandboxState, EngineError> {
        self.lane().transition(Transition::Resume, |p| p.resume())
    }

    /// Back to `created`, keeping the sandbox and its configuration.
    ///
    /// The identity survives, so a recording shows one sandbox reset, not two
    /// that look alike.
    pub fn reset(&self) -> Result<SandboxState, EngineError> {
        self.lane().transition(Transition::Reset, |p| p.reset())
    }

    /// Take it away and give the allocation back.
    ///
    /// Released here, where the provider is known to have let go. A failed
    /// sandbox holds its resources until destroyed: a dead guest still has a
    /// disk.
    pub fn destroy(&self) -> Result<SandboxState, EngineError> {
        let mut core = self.lane();
        let state = core.transition(Transition::Destroy, |p| p.destroy())?;
        let id = core.id();
        lock(&core.ledger).release(&id);
        Ok(state)
    }

    /// Any of the six a caller can ask for, by value.
    pub fn apply(&self, transition: Transition) -> Result<SandboxState, EngineError> {
        match transition {
            Transition::Start => self.start(),
            Transition::Stop => self.stop(),
            Transition::Pause => self.pause(),
            Transition::Resume => self.resume(),
            Transition::Reset => self.reset(),
            Transition::Destroy => self.destroy(),
            Transition::Fail => {
                Err(ContractError::broken("'fail' is not a transition anybody can ask for").into())
            }
        }
    }

    /// Replace the configuration with `config`. The sandbox stays put.
    ///
    /// Refused while running, for another type, or without
    /// `lifecycle.reconfigure`. The ledger goes first, so a budget that can't
    /// cover it refuses whole. If the provider refuses, nothing changes.
    pub fn reconfigure(&self, config: SandboxConfig) -> Result<SandboxState, EngineError> {
        self.lane().reconfigure(config)
    }

    /// Call it something else, in any state, without asking the provider.
    /// Empty means the type's name. Says `sandbox.renamed`.
    pub fn rename(&self, name: &str) {
        self.lane().rename(name);
    }

    /// Point it at other sandboxes (`SandboxRuntime::reuse`, which has
    /// already checked them). Only while created or stopped.
    pub(crate) fn rebind(
        &self,
        uses: std::collections::BTreeMap<String, String>,
        peers: crate::runtime::peers::Peers,
    ) -> Result<SandboxState, EngineError> {
        self.lane().rebind(uses, peers)
    }

    // --- asking what is true ------------------------------------------------------

    /// Ask the provider what it can see, and reconcile it with the lifecycle.
    ///
    /// One direction only: "it's gone" moves a live sandbox to `failed`, but
    /// "it's there" never resurrects one. Leaving `failed` is a caller's
    /// decision.
    ///
    /// A refresh that waited behind the lane takes an observation that began
    /// after it asked, instead of asking the guest twice. One that began
    /// before isn't new enough.
    pub fn refresh(&self) -> Result<SandboxState, EngineError> {
        let asked = self.looks.begun.load(Ordering::SeqCst);
        let mut core = self.lane();
        if self.looks.answered.load(Ordering::SeqCst) > asked {
            return Ok(core.state());
        }
        let number = self.looks.begun.fetch_add(1, Ordering::SeqCst) + 1;
        let state = core.refresh()?;
        self.looks.answered.store(number, Ordering::SeqCst);
        Ok(state)
    }

    // --- asking it to do something -----------------------------------------------

    /// `sandbox.act("process.execute", data)`. A broken contract only for a
    /// capability that isn't `namespace.verb`; anything else is an outcome.
    pub fn act(&self, capability: &str, arguments: Data) -> Result<ActionOutcome, ContractError> {
        Ok(self.perform(Action::new(capability, arguments)?))
    }

    /// Run one action, and report what came of it rather than failing.
    ///
    /// The runtime checks the sandbox exists, the capability was declared and
    /// the arguments fit the spec. Whether now is a good moment is the
    /// provider's call.
    pub fn perform(&self, action: Action) -> ActionOutcome {
        self.lane().perform(action)
    }
}

impl fmt::Display for Sandbox {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let view = read(&self.view);
        write!(formatter, "{} [{}]", view.identity, view.state.lifecycle)
    }
}

impl fmt::Debug for Sandbox {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Sandbox({self})")
    }
}

impl Core {
    fn id(&self) -> String {
        read(&self.view).identity.id.clone()
    }

    fn type_name(&self) -> String {
        read(&self.view).identity.type_name.clone()
    }

    fn lifecycle(&self) -> Lifecycle {
        read(&self.view).state.lifecycle
    }

    fn state(&self) -> SandboxState {
        read(&self.view).state.clone()
    }

    fn supports(&self, capability: &str) -> bool {
        read(&self.view).capabilities.supports(capability)
    }

    fn transition(
        &mut self,
        transition: Transition,
        work: impl FnOnce(&mut dyn SandboxProvider) -> Result<(), ProviderError>,
    ) -> Result<SandboxState, EngineError> {
        let current = self.lifecycle();
        let Some(target) = next_state(current, transition) else {
            return Err(self.refuse(
                reason!(
                    "engine.bad_transition",
                    state = current,
                    transition = transition
                ),
                json_data(&[("transition", json!(transition.as_str()))]),
            ));
        };
        if let Some(capability) = gate(transition)
            && !self.supports(capability)
        {
            return Err(self.refuse(
                reason!("engine.unsupported", capability = capability),
                json_data(&[("transition", json!(transition.as_str()))]),
            ));
        }

        let provider = self.provider.as_mut();
        match catch_unwind(AssertUnwindSafe(|| work(provider))) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.broken(transition.as_str(), &error);
                return Err(error.into());
            }
            Err(payload) => {
                let detail = panic_text(payload.as_ref());
                log::error!(
                    "BeatBox sandbox {} failed to {transition}: {detail}",
                    self.id()
                );
                let failure = ProviderError::new(provider_failed(
                    &self.type_name(),
                    transition.as_str(),
                    &detail,
                ));
                self.broken(transition.as_str(), &failure);
                return Err(failure.into());
            }
        }

        self.settle(target, None);
        self.say(event_for(transition), Data::new());
        if target.is_live() {
            self.observe_quietly();
        }
        Ok(self.state())
    }

    fn reconfigure(&mut self, config: SandboxConfig) -> Result<SandboxState, EngineError> {
        let current = self.lifecycle();
        let refusal = json_data(&[("transition", json!("reconfigure"))]);
        if !RECONFIGURABLE.contains(&current) {
            return Err(self.refuse(
                reason!("engine.reconfigure_not_now", state = current),
                refusal,
            ));
        }
        if !self.supports(LIFECYCLE_RECONFIGURE) {
            return Err(self.refuse(
                reason!("engine.unsupported", capability = LIFECYCLE_RECONFIGURE),
                refusal,
            ));
        }
        let previous = read(&self.view).config.clone();
        if config.type_name() != previous.type_name() {
            return Err(self.refuse(
                reason!("engine.reconfigure_type", type = previous.type_name()),
                refusal,
            ));
        }
        // Who fills a role is fixed at creation (`peers.rs`). Saying nothing
        // keeps them; saying something else is refused.
        let mut config = config;
        if config.uses.is_empty() {
            config.uses = previous.uses.clone();
        } else if config.uses != previous.uses {
            return Err(self.refuse(reason!("engine.peer_fixed"), refusal));
        }

        let id = self.id();
        {
            let mut ledger = lock(&self.ledger);
            ledger.release(&id);
            if let Err(refused) = ledger.allocate(&id, &config.resources) {
                ledger
                    .allocate(&id, &previous.resources)
                    .expect("what was just released fits again");
                return Err(refused);
            }
        }
        let provider = self.provider.as_mut();
        let outcome = match catch_unwind(AssertUnwindSafe(|| provider.reconfigure(&config))) {
            Ok(result) => result,
            Err(payload) => {
                let detail = panic_text(payload.as_ref());
                log::error!("BeatBox sandbox {id} failed to reconfigure: {detail}");
                Err(ProviderError::new(provider_failed(
                    &self.type_name(),
                    "reconfigure",
                    &detail,
                )))
            }
        };
        if let Err(error) = outcome {
            {
                let mut ledger = lock(&self.ledger);
                ledger.release(&id);
                ledger
                    .allocate(&id, &previous.resources)
                    .expect("what was just released fits again");
            }
            self.broken("reconfigure", &error);
            return Err(error.into());
        }
        let written = config.to_value();
        write(&self.view).config = config;
        self.say("sandbox.reconfigured", json_data(&[("config", written)]));
        self.observe_quietly();
        Ok(self.state())
    }

    fn rebind(
        &mut self,
        uses: std::collections::BTreeMap<String, String>,
        peers: crate::runtime::peers::Peers,
    ) -> Result<SandboxState, EngineError> {
        let current = self.lifecycle();
        if !RECONFIGURABLE.contains(&current) {
            return Err(self.refuse(
                reason!("engine.reconfigure_not_now", state = current),
                json_data(&[("transition", json!("reuse"))]),
            ));
        }
        self.provider.given(peers);
        let written = {
            let mut view = write(&self.view);
            view.config.uses = uses;
            view.config.to_value()
        };
        self.say("sandbox.reconfigured", json_data(&[("config", written)]));
        self.observe_quietly();
        Ok(self.state())
    }

    fn rename(&mut self, name: &str) {
        let asked = name.trim();
        let shown = if asked.is_empty() {
            self.type_name()
        } else {
            asked.to_owned()
        };
        {
            let mut view = write(&self.view);
            view.identity.name.clone_from(&shown);
            view.config.name = asked.to_owned();
        }
        self.say("sandbox.renamed", json_data(&[("name", json!(shown))]));
    }

    fn refresh(&mut self) -> Result<SandboxState, EngineError> {
        let current = self.lifecycle();
        if current == Lifecycle::Destroyed {
            return Ok(self.state());
        }
        let provider = self.provider.as_mut();
        let observation = match catch_unwind(AssertUnwindSafe(|| provider.observe())) {
            Ok(observation) => observation,
            Err(payload) => {
                let detail = panic_text(payload.as_ref());
                log::error!("BeatBox sandbox {} failed to observe: {detail}", self.id());
                return Err(ProviderError::new(provider_failed(
                    &self.type_name(),
                    "observe",
                    &detail,
                ))
                .into());
            }
        };
        if observation.alive == Some(false)
            && observation.halted
            && allows(current, Transition::Stop)
        {
            self.settle(Lifecycle::Stopped, observation.reason);
            let why = reason_data(self.state().reason.as_ref());
            self.say(event_for(Transition::Stop), why);
            return Ok(self.state());
        }
        if observation.alive == Some(false) && allows(current, Transition::Fail) {
            let type_name = self.type_name();
            let why = observation
                .reason
                .unwrap_or_else(|| reason!("engine.sandbox_lost", type = type_name));
            self.fail(Some(why));
            return Ok(self.state());
        }
        let mut view = write(&self.view);
        view.state = view.state.observed(observation);
        Ok(view.state.clone())
    }

    fn perform(&mut self, action: Action) -> ActionOutcome {
        let current = self.lifecycle();
        if !current.is_live() {
            return self.declined(action, reason!("engine.cannot_act", state = current));
        }
        if !self.supports(action.capability()) {
            let why = reason!("engine.unsupported", capability = action.capability());
            return self.declined(action, why);
        }
        let spec = self
            .actions
            .iter()
            .find(|spec| spec.capability == action.capability());
        if let Some(Err(why)) = spec.map(|spec| spec.check_inputs(&action.arguments)) {
            return self.declined(action, why);
        }

        // Record the action before settling the loss, or the story says the
        // sandbox died before the thing that killed it.
        let mut lost = None;
        let provider = self.provider.as_mut();
        let outcome = match catch_unwind(AssertUnwindSafe(|| provider.perform(&action))) {
            Ok(Ok(data)) => ActionOutcome::done(action, data),
            Ok(Err(error)) => {
                if error.fatal {
                    lost = Some(error.reason.clone());
                }
                ActionOutcome::failed(action, error.reason)
            }
            Err(payload) => {
                let detail = panic_text(payload.as_ref());
                log::error!(
                    "BeatBox sandbox {} failed performing {}: {detail}",
                    self.id(),
                    action.capability()
                );
                let why = provider_failed(&self.type_name(), action.capability(), &detail);
                ActionOutcome::failed(action, Some(why))
            }
        };

        let said = json_data(&[
            ("capability", json!(outcome.capability())),
            ("ok", json!(outcome.ok)),
        ]);
        self.say(
            "sandbox.action",
            merged(said, reason_data(outcome.reason.as_ref())),
        );
        if let Some(why) = lost {
            self.fail(why);
        }
        outcome
    }

    /// Record an attempt that did not complete.
    ///
    /// `sandbox.refused`: the runtime said no first. `sandbox.broke`: the
    /// provider failed and the sandbox survived. `sandbox.failed`: it didn't.
    fn broken(&mut self, transition: &str, error: &ProviderError) {
        if error.fatal {
            self.fail(error.reason.clone());
            return;
        }
        let said = json_data(&[("transition", json!(transition))]);
        self.say(
            "sandbox.broke",
            merged(said, reason_data(error.reason.as_ref())),
        );
    }

    /// Mark a sandbox the world took away. Never re-observes: this is called
    /// from an observation.
    fn fail(&mut self, why: Option<Reason>) {
        if !allows(self.lifecycle(), Transition::Fail) {
            return;
        }
        let type_name = self.type_name();
        let why = why.unwrap_or_else(|| reason!("engine.sandbox_lost", type = type_name));
        self.settle(Lifecycle::Failed, Some(why));
        let said = reason_data(self.state().reason.as_ref());
        self.say(event_for(Transition::Fail), said);
    }

    fn settle(&mut self, lifecycle: Lifecycle, why: Option<Reason>) {
        let since = (self.clock)();
        write(&self.view).state = SandboxState::new(lifecycle, since, why);
    }

    fn observe_quietly(&mut self) {
        if let Err(error) = self.refresh() {
            log::error!(
                "BeatBox sandbox {} could not be observed after a transition: {error}",
                self.id()
            );
        }
    }

    /// Say no, out loud, and hand back the refusal to return.
    fn refuse(&mut self, why: Reason, data: Data) -> EngineError {
        self.say("sandbox.refused", merged(data, reason_data(Some(&why))));
        EngineError::Refused(why)
    }

    fn declined(&mut self, action: Action, why: Reason) -> ActionOutcome {
        let said = json_data(&[("capability", json!(action.capability()))]);
        self.say("sandbox.refused", merged(said, reason_data(Some(&why))));
        ActionOutcome::failed(action, Some(why))
    }

    fn say(&self, name: &str, data: Data) {
        self.publish
            .emit(name, data)
            .expect("the runtime's own event names are valid");
    }
}

/// A little event data, in the order it is written.
pub(crate) fn json_data(pairs: &[(&str, Value)]) -> Data {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}
