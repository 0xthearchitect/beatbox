//! The runtime: it creates sandboxes, holds them, and owns nothing else.
//!
//! Its four parts are handed in, so a test, a demo and the app each build
//! their own:
//!
//! ```text
//! registry   which types can be built at all
//! events     where everything that happens is published
//! resources  the ledger of what has been promised to whom
//! clock/ids  so that a test can have a boring, repeatable story
//! ```
//!
//! **Creating either finishes or leaves nothing behind.** If the provider's
//! `create` fails, it destroys what it made and the allocation goes back;
//! otherwise a half-made sandbox holds four cores until the fifth won't start.
//!
//! **Destroyed sandboxes are kept.** [`SandboxRuntime::sandboxes`] skips
//! them, but [`SandboxRuntime::get`] still answers "what happened to it".

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use beatbox_reasons::{Reason, reason};
use serde_json::json;

use crate::clock::{Clock, Ids, random_ids, system_clock};
use crate::events::EventBus;
use crate::events::bus::lock;
use crate::faults::{ContractError, EngineError, ProviderError, repr};
use crate::runtime::config::SandboxConfig;
use crate::runtime::identity::SandboxIdentity;
use crate::runtime::isolation::at_least;
use crate::runtime::lifecycle::Lifecycle;
use crate::runtime::peers::{Peers, Requirement};
use crate::runtime::provider::ProviderSpec;
use crate::runtime::provider::{Emitter, SandboxProvider};
use crate::runtime::registry::{SandboxRegistry, SandboxType};
use crate::runtime::resources::ResourceLedger;
use crate::runtime::sandbox::{Parts, Sandbox, json_data, panic_text, provider_failed};
use crate::runtime::state::SandboxState;

/// What a runtime is assembled from. Everything left out is the default: an
/// empty registry, a bus on the runtime's clock, an unbounded ledger, the
/// system's clock, random ids.
#[derive(Default)]
pub struct RuntimeParts {
    /// Which types can be built.
    pub registry: SandboxRegistry,
    /// Where events go. Default: a bus stamped by `clock`.
    pub events: Option<Arc<EventBus>>,
    /// Who has been promised what.
    pub ledger: ResourceLedger,
    /// Where moments come from.
    pub clock: Option<Clock>,
    /// Where ids come from.
    pub ids: Option<Ids>,
}

/// Create, find and destroy sandboxes of any type, knowing none of them.
///
/// Every method takes `&self`. The registry and the sandbox list have their
/// own locks, held briefly and never across a provider's work, so the daemon
/// can answer from several threads and a subscriber can find a sandbox
/// while it's still being created.
pub struct SandboxRuntime {
    registry: RwLock<SandboxRegistry>,
    events: Arc<EventBus>,
    resources: Arc<Mutex<ResourceLedger>>,
    clock: Clock,
    ids: Ids,
    sandboxes: Arc<RwLock<Vec<Sandbox>>>,
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

impl Default for SandboxRuntime {
    fn default() -> Self {
        Self::new(RuntimeParts::default())
    }
}

impl SandboxRuntime {
    /// A runtime made of `parts`.
    pub fn new(parts: RuntimeParts) -> Self {
        let clock = parts.clock.unwrap_or_else(system_clock);
        let events = parts
            .events
            .unwrap_or_else(|| Arc::new(EventBus::new(Arc::clone(&clock))));
        Self {
            registry: RwLock::new(parts.registry),
            events,
            resources: Arc::new(Mutex::new(parts.ledger)),
            clock,
            ids: parts.ids.unwrap_or_else(random_ids),
            sandboxes: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Which types can be built. Read it and drop the guard.
    pub fn registry(&self) -> RwLockReadGuard<'_, SandboxRegistry> {
        read(&self.registry)
    }

    /// Make another type buildable here. See [`SandboxRegistry::register`].
    pub fn register(&self, spec: ProviderSpec) -> Result<SandboxType, ContractError> {
        write(&self.registry).register(spec)
    }

    /// The same, replacing a type of the same name.
    pub fn register_replacing(&self, spec: ProviderSpec) -> Result<SandboxType, ContractError> {
        write(&self.registry).register_replacing(spec)
    }

    /// Where everything that happens is published.
    pub fn events(&self) -> &Arc<EventBus> {
        &self.events
    }

    /// The ledger. Read it and drop the guard.
    pub fn resources(&self) -> MutexGuard<'_, ResourceLedger> {
        lock(&self.resources)
    }

    // --- making one ----------------------------------------------------------

    /// Build one sandbox from one configuration, or leave nothing behind.
    pub fn create(&self, mut config: SandboxConfig) -> Result<Sandbox, EngineError> {
        let entry = self.registry().get(config.type_name())?;

        // Before anything is built: a weaker boundary than asked for is the
        // one failure BeatBox can't make quietly.
        if !at_least(entry.isolation, config.isolation.level) {
            return Err(EngineError::Refused(reason!(
                "engine.isolation_too_weak",
                type = entry.name,
                provided = entry.isolation,
                required = config.isolation.level,
            )));
        }

        // Checked before anything is allocated, and kept by id from here on
        // so a rename doesn't lose a peer.
        let filled = self.fill(&entry.spec().requires, &config.uses, None)?;
        config.uses = filled
            .iter()
            .filter_map(|(requirement, id)| Some((requirement.role.clone(), id.clone()?)))
            .collect();

        let name = if config.name.is_empty() {
            entry.name.clone()
        } else {
            config.name.clone()
        };
        let identity = SandboxIdentity::new(&(self.ids)(), &entry.name, &name, &entry.version);
        if self.contains(&identity.id) {
            return Err(ContractError::broken(format!(
                "the id generator handed out {} twice",
                identity.id
            ))
            .into());
        }

        lock(&self.resources).allocate(&identity.id, &config.resources)?;
        let mut provider = entry.spec().build(
            identity.clone(),
            config.clone(),
            self.emitter(&identity.id, false),
        );
        if !filled.is_empty() {
            provider.given(Peers::new(filled, Arc::downgrade(&self.sandboxes)));
        }
        let created = match catch_unwind(AssertUnwindSafe(|| provider.create())) {
            Ok(result) => result,
            Err(payload) => {
                let detail = panic_text(payload.as_ref());
                log::error!("BeatBox sandbox {} failed to create: {detail}", identity.id);
                Err(ProviderError::new(provider_failed(
                    &identity.type_name,
                    "create",
                    &detail,
                )))
            }
        };
        if let Err(error) = created {
            self.unmake(provider.as_mut(), &identity);
            return Err(error.into());
        }

        let sandbox = Sandbox::new(Parts {
            identity: identity.clone(),
            config,
            declared: entry.capabilities.clone(),
            actions: entry.spec().actions.clone(),
            provider,
            publish: self.emitter(&identity.id, true),
            ledger: Arc::clone(&self.resources),
            clock: Arc::clone(&self.clock),
            state: None,
        });
        write(&self.sandboxes).push(sandbox.clone());
        self.events
            .publish(
                "sandbox.created",
                &identity.id,
                json_data(&[
                    ("type", json!(identity.type_name)),
                    ("name", json!(identity.name)),
                    ("version", json!(identity.version)),
                    ("isolation", json!(entry.isolation.as_str())),
                    ("capabilities", json!(sandbox.capabilities().to_list())),
                ]),
            )
            .expect("sandbox.created is a valid name");
        // Hand back an observed state, like every transition does. Best
        // effort: the sandbox exists whether or not it can be described now.
        if let Err(error) = sandbox.refresh() {
            log::error!(
                "BeatBox sandbox {} could not be observed after being created: {error}",
                identity.id
            );
        }
        Ok(sandbox)
    }

    /// Bring back a sandbox this runtime did not create.
    ///
    /// After the app restarts, with the guest's disk still in place. Not
    /// [`SandboxRuntime::create`]: a host's `create()` makes a fresh overlay,
    /// which is exactly what this must not throw away.
    ///
    /// The identity is kept as given. `lifecycle_hint` holds until the first
    /// observation; a provider that finds nothing fails the sandbox there.
    pub fn restore(
        &self,
        identity: SandboxIdentity,
        config: SandboxConfig,
        lifecycle_hint: Lifecycle,
        reason_hint: Option<Reason>,
    ) -> Result<Sandbox, EngineError> {
        let entry = self.registry().get(config.type_name())?;
        if self.contains(&identity.id) {
            return Err(ContractError::broken(format!(
                "sbx {} is already known to this runtime",
                identity.id
            ))
            .into());
        }
        lock(&self.resources).allocate(&identity.id, &config.resources)?;
        let mut provider = entry.spec().build(
            identity.clone(),
            config.clone(),
            self.emitter(&identity.id, false),
        );
        // Checked at creation already; one gone since answers
        // `engine.peer_gone` when asked.
        let filled: Vec<(Requirement, Option<String>)> = entry
            .spec()
            .requires
            .iter()
            .map(|requirement| {
                let id = config.uses.get(&requirement.role).cloned();
                (requirement.clone(), id)
            })
            .collect();
        if !filled.is_empty() {
            provider.given(Peers::new(filled, Arc::downgrade(&self.sandboxes)));
        }
        let state = SandboxState::new(lifecycle_hint, (self.clock)(), reason_hint);
        let sandbox = Sandbox::new(Parts {
            identity: identity.clone(),
            config,
            declared: entry.capabilities.clone(),
            actions: entry.spec().actions.clone(),
            provider,
            publish: self.emitter(&identity.id, true),
            ledger: Arc::clone(&self.resources),
            clock: Arc::clone(&self.clock),
            state: Some(state),
        });
        write(&self.sandboxes).push(sandbox.clone());
        if let Err(error) = sandbox.refresh() {
            log::error!(
                "BeatBox sandbox {} could not be observed after being restored: {error}",
                identity.id
            );
        }
        Ok(sandbox)
    }

    /// Who fills each role `requires` names, by id, or why not.
    ///
    /// `own` is the sandbox being re-pointed, when there is one: it may not
    /// be its own peer, nor use one that already uses it.
    fn fill(
        &self,
        requires: &[Requirement],
        uses: &BTreeMap<String, String>,
        own: Option<&str>,
    ) -> Result<Vec<(Requirement, Option<String>)>, EngineError> {
        let refused = |why| Err(EngineError::Refused(why));
        if let Some(role) = uses
            .keys()
            .find(|role| !requires.iter().any(|r| &r.role == *role))
        {
            return refused(reason!("engine.peer_unexpected", role = role));
        }
        let live = self.sandboxes();
        let mut filled = Vec::new();
        for requirement in requires {
            let role = &requirement.role;
            let Some(target) = uses.get(role).filter(|t| !t.is_empty()) else {
                if requirement.optional {
                    filled.push((requirement.clone(), None));
                    continue;
                }
                return refused(reason!("engine.peer_missing", role = role));
            };
            // An id first; a name only when exactly one sandbox has it.
            let named: Vec<&Sandbox> = live.iter().filter(|s| s.name() == *target).collect();
            let found = live.iter().find(|s| s.id() == *target).or(match named[..] {
                [one] => Some(one),
                _ => None,
            });
            let Some(peer) = found else {
                return refused(reason!("engine.peer_unknown", role = role, target = target));
            };
            if let Some(own) = own {
                if peer.id() == own {
                    return refused(reason!("engine.peer_self", role = role));
                }
                if self.reaches(&peer.id(), own) {
                    return refused(reason!(
                        "engine.peer_cycle",
                        role = role,
                        peer = peer.name()
                    ));
                }
            }
            let can = peer.capabilities();
            if let Some(missing) = requirement.capabilities.iter().find(|c| !can.supports(c)) {
                return refused(reason!(
                    "engine.peer_lacks",
                    role = role,
                    peer = peer.name(),
                    capability = missing,
                ));
            }
            filled.push((requirement.clone(), Some(peer.id())));
        }
        Ok(filled)
    }

    /// Whether `from` uses `target`, directly or through what it uses.
    fn reaches(&self, from: &str, target: &str) -> bool {
        let live = self.sandboxes();
        let mut seen = vec![from.to_owned()];
        let mut next = vec![from.to_owned()];
        while let Some(at) = next.pop() {
            let Some(sandbox) = live.iter().find(|s| s.id() == at) else {
                continue;
            };
            for peer in sandbox.config().uses.values() {
                if peer == target {
                    return true;
                }
                if !seen.contains(peer) {
                    seen.push(peer.clone());
                    next.push(peer.clone());
                }
            }
        }
        false
    }

    /// Point a sandbox at other peers, checked as at creation, plus never
    /// itself and never a cycle. Only while created or stopped, so a
    /// Server's services aren't running on the old ones.
    pub fn reuse(
        &self,
        sandbox_id: &str,
        uses: &BTreeMap<String, String>,
    ) -> Result<Sandbox, EngineError> {
        let sandbox = self.get(sandbox_id)?;
        let entry = self.registry().get(&sandbox.type_name())?;
        let filled = self.fill(&entry.spec().requires, uses, Some(sandbox_id))?;
        let written: BTreeMap<String, String> = filled
            .iter()
            .filter_map(|(requirement, id)| Some((requirement.role.clone(), id.clone()?)))
            .collect();
        let peers = Peers::new(filled, Arc::downgrade(&self.sandboxes));
        sandbox.rebind(written, peers)?;
        Ok(sandbox)
    }

    /// Undo a creation that failed half way through.
    ///
    /// Why `destroy` has to survive a sandbox that was never fully created.
    fn unmake(&self, provider: &mut dyn SandboxProvider, identity: &SandboxIdentity) {
        match catch_unwind(AssertUnwindSafe(|| provider.destroy())) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => log::error!(
                "BeatBox could not clean up {} after a failed create: {error}",
                identity.id
            ),
            Err(payload) => log::error!(
                "BeatBox could not clean up {} after a failed create: {}",
                identity.id,
                panic_text(payload.as_ref())
            ),
        }
        lock(&self.resources).release(&identity.id);
        self.events
            .publish(
                "sandbox.refused",
                &identity.id,
                json_data(&[
                    ("transition", json!("create")),
                    ("type", json!(identity.type_name)),
                ]),
            )
            .expect("sandbox.refused is a valid name");
    }

    // --- finding them --------------------------------------------------------

    /// The sandbox with this id, destroyed ones included.
    pub fn get(&self, sandbox_id: &str) -> Result<Sandbox, ContractError> {
        read(&self.sandboxes)
            .iter()
            .find(|sandbox| sandbox.id() == sandbox_id)
            .cloned()
            .ok_or_else(|| {
                ContractError::UnknownSandbox(format!("no sandbox with id {}", repr(sandbox_id)))
            })
    }

    /// The live ones, in creation order.
    pub fn sandboxes(&self) -> Vec<Sandbox> {
        read(&self.sandboxes)
            .iter()
            .filter(|sandbox| sandbox.lifecycle() != Lifecycle::Destroyed)
            .cloned()
            .collect()
    }

    /// Every one this runtime has held, destroyed ones included.
    pub fn every_sandbox(&self) -> Vec<Sandbox> {
        read(&self.sandboxes).clone()
    }

    /// Live sandboxes that declared `capability`.
    ///
    /// [`SandboxRegistry::providing`] says what could be built; this says
    /// what's already here. `running` narrows to what can be asked now.
    pub fn providing(&self, capability: &str, running: bool) -> Vec<Sandbox> {
        self.sandboxes()
            .into_iter()
            .filter(|sandbox| {
                sandbox.capabilities().supports(capability)
                    && (!running || sandbox.lifecycle() == Lifecycle::Running)
            })
            .collect()
    }

    /// Whether this runtime has ever held a sandbox with this id.
    pub fn contains(&self, sandbox_id: &str) -> bool {
        read(&self.sandboxes)
            .iter()
            .any(|sandbox| sandbox.id() == sandbox_id)
    }

    /// How many are live.
    pub fn len(&self) -> usize {
        self.sandboxes().len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // --- taking them away ----------------------------------------------------

    /// Destroy every live sandbox, newest first, and report what would not go.
    ///
    /// Newest first, since later ones tend to use earlier ones. Doesn't stop
    /// at the first stubborn guest, or the rest keep running.
    pub fn destroy_all(&self) -> Vec<String> {
        let mut stubborn = Vec::new();
        for sandbox in self.sandboxes().into_iter().rev() {
            if let Err(error) = sandbox.destroy() {
                log::error!("BeatBox could not destroy {}: {error}", sandbox.id());
                stubborn.push(sandbox.id());
            }
        }
        stubborn
    }

    fn emitter(&self, sandbox_id: &str, reserved_ok: bool) -> Emitter {
        Emitter::new(sandbox_id, Arc::clone(&self.events), reserved_ok)
    }
}
