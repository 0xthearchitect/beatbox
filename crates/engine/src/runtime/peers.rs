//! A sandbox that uses another, without knowing what the other is.
//!
//! A type names the roles it needs ([`Requirement`]), a configuration says
//! who fills them ([`SandboxConfig::uses`](crate::SandboxConfig::uses)), and
//! the provider gets [`Peers`]. Acting on a peer goes through the same gates
//! as any caller, so a Server runs commands in a Host without importing it.
//!
//! - **Only what was declared** (`engine.peer_undeclared`). A requirement
//!   can't name `lifecycle.*`: a peer's lifecycle belongs to whoever put the
//!   two together.
//! - **Fixed at creation** (`engine.peer_fixed`). A peer exists first, so two
//!   sandboxes can't use each other and no two lanes wait on each other.
//! - **A peer that goes isn't held back.** Its user hears `engine.peer_gone`
//!   on the next ask.
//! - **A role may be optional.** Left empty it answers `engine.peer_absent`:
//!   nothing was ever there.
//! - **Held by id, found when asked**, so after a restart it doesn't matter
//!   which came back first.

use std::collections::BTreeMap;
use std::sync::{PoisonError, RwLock, Weak};

use beatbox_reasons::reason;

use crate::events::Data;
use crate::faults::ContractError;
use crate::runtime::actions::{Action, ActionOutcome};
use crate::runtime::lifecycle::Lifecycle;
use crate::runtime::sandbox::Sandbox;
use crate::runtime::state::SandboxState;

/// One role a type needs filled, and what the sandbox in it has to be able
/// to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// What the role is called: one lowercase word, `host`.
    pub role: String,
    /// What the sandbox in it must declare, and all its user may ask of it.
    pub capabilities: Vec<String>,
    /// Whether the role may be left empty. An empty one answers
    /// `engine.peer_absent`; a filled one is checked like any other.
    pub optional: bool,
}

impl Requirement {
    /// `role`, filled by something that can do all of `capabilities`.
    pub fn new(role: &str, capabilities: &[&str]) -> Self {
        Self {
            role: role.to_owned(),
            capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
            optional: false,
        }
    }

    /// The same, allowed to be left empty.
    #[must_use]
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

/// Every sandbox a runtime holds, as a peer finds them: weakly, so a
/// sandbox that uses another does not keep the runtime's list alive.
pub(crate) type Held = Weak<RwLock<Vec<Sandbox>>>;

/// The sandboxes one sandbox uses, by role.
///
/// Empty for a type that requires none. Each role holds the peer's id and
/// looks it up on demand.
#[derive(Clone, Default)]
pub struct Peers {
    roles: BTreeMap<String, (Requirement, Option<String>)>,
    held: Held,
}

impl Peers {
    pub(crate) fn new(filled: Vec<(Requirement, Option<String>)>, held: Held) -> Self {
        Self {
            roles: filled
                .into_iter()
                .map(|(requirement, id)| (requirement.role.clone(), (requirement, id)))
                .collect(),
            held,
        }
    }

    /// The sandbox in `role`, if it is filled and still there.
    fn find(&self, role: &str) -> Option<Sandbox> {
        let id = self.roles.get(role)?.1.as_ref()?;
        let held = self.held.upgrade()?;
        let all = held.read().unwrap_or_else(PoisonError::into_inner);
        all.iter()
            .find(|sandbox| sandbox.id() == *id && sandbox.lifecycle() != Lifecycle::Destroyed)
            .cloned()
    }

    /// The name of the sandbox in `role`, as the person called it.
    pub fn name(&self, role: &str) -> Option<String> {
        self.find(role).map(|sandbox| sandbox.name())
    }

    /// Ask the sandbox in `role` to do `capability`.
    ///
    /// A broken contract only for a capability that is not `namespace.verb`;
    /// everything else is an outcome, refusals included.
    pub fn act(
        &self,
        role: &str,
        capability: &str,
        arguments: Data,
    ) -> Result<ActionOutcome, ContractError> {
        let action = Action::new(capability, arguments)?;
        let declared = self.roles.get(role).is_some_and(|(requirement, _)| {
            requirement.capabilities.iter().any(|c| c == capability)
        });
        if !declared {
            let why = reason!(
                "engine.peer_undeclared",
                role = role,
                capability = capability
            );
            return Ok(ActionOutcome::failed(action, Some(why)));
        }
        // Let go of the list before asking: its lock never spans a
        // provider's work.
        match self.find(role) {
            Some(sandbox) => Ok(sandbox.perform(action)),
            None if self.id(role).is_none() => Ok(ActionOutcome::failed(
                action,
                Some(reason!("engine.peer_absent", role = role)),
            )),
            None => Ok(ActionOutcome::failed(
                action,
                Some(reason!("engine.peer_gone", role = role)),
            )),
        }
    }

    /// What was last seen of the sandbox in `role` -- its `facts` included --
    /// or `None` when there is none.
    pub fn state(&self, role: &str) -> Option<SandboxState> {
        self.find(role).map(|sandbox| sandbox.state())
    }

    /// The id of the sandbox that fills `role`, whether or not it is still
    /// there -- `None` for a role left empty, or one this type never named.
    pub fn id(&self, role: &str) -> Option<&str> {
        self.roles.get(role)?.1.as_deref()
    }
}

impl std::fmt::Debug for Peers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let filled: BTreeMap<&String, &Option<String>> = self
            .roles
            .iter()
            .map(|(role, (_, id))| (role, id))
            .collect();
        formatter.debug_tuple("Peers").field(&filled).finish()
    }
}
