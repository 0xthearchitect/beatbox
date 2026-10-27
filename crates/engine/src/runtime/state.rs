//! What is true. Never what was asked for -- that is next door.
//!
//! [`Observation`] is the provider's reading, not stored. [`SandboxState`]
//! is the runtime's account: lifecycle, since when, why, plus the last
//! reading.
//!
//! **A transition clears what was observed.** A guest's addresses from
//! before a restart aren't its addresses now, so the runtime observes again.

use beatbox_reasons::Reason;
use serde_json::Value;

use crate::events::Data;
use crate::runtime::lifecycle::Lifecycle;
use crate::runtime::resources::{NOTHING, Resources};

/// What a provider can see right now.
///
/// `alive`: is it still there, in the shape its lifecycle claims? A stopped
/// guest is alive; one whose QEMU is gone isn't. `None` is "can't tell", and
/// the runtime never acts on it, or quiet sandboxes would fail.
///
/// `halted` qualifies a `false`: shut down from inside on purpose, so it goes
/// to `stopped`, not `failed`. Someone choosing Shut down in Windows isn't a
/// crash.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Observation {
    /// Still there, gone, or cannot tell.
    pub alive: Option<bool>,
    /// Gone on purpose, from inside.
    pub halted: bool,
    /// What it is using.
    pub resources: Resources,
    /// Whatever the provider knows, in its own vocabulary.
    pub facts: Data,
    /// Why it is gone, when that is known.
    pub reason: Option<Reason>,
}

/// Where a sandbox is, since when, and what was last seen of it.
///
/// `facts` is the provider's vocabulary; the runtime carries it unread. A few
/// keys mean the same in every type, so a reader needn't ask which kind:
///
/// | Key | Shape | Said by |
/// |---|---|---|
/// | `addresses` | a list of IP addresses as text, no prefix | Host, from its agent |
/// | `endpoints` | a list of `{service, protocol, address, port}` | Server |
///
/// A key joins only when a second type says it or another reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxState {
    /// The runtime's word for where it is. The only thing anything branches on.
    pub lifecycle: Lifecycle,
    /// When it got there, by the runtime's clock.
    pub since: String,
    /// Why, when that needs saying: a failure, a halt.
    pub reason: Option<Reason>,
    /// What it was last seen using.
    pub resources: Resources,
    /// What was last seen of it.
    pub facts: Data,
}

impl SandboxState {
    /// A state as a transition leaves it: nothing observed yet.
    pub fn new(lifecycle: Lifecycle, since: String, reason: Option<Reason>) -> Self {
        Self {
            lifecycle,
            since,
            reason,
            resources: NOTHING,
            facts: Data::new(),
        }
    }

    /// One fact, if the last observation reported it.
    pub fn fact(&self, key: &str) -> Option<&Value> {
        self.facts.get(key)
    }

    /// The same lifecycle, with a fresh reading of everything else.
    ///
    /// Lifecycle, moment and reason are untouched: even a lost sandbox
    /// changes them through a transition, with its event.
    #[must_use]
    pub fn observed(&self, observation: Observation) -> Self {
        Self {
            lifecycle: self.lifecycle,
            since: self.since.clone(),
            reason: self.reason.clone(),
            resources: observation.resources,
            facts: observation.facts,
        }
    }

    /// As JSON, the shape the records keep.
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "lifecycle": self.lifecycle.as_str(),
            "since": self.since,
            "reason": self.reason,
            "resources": self.resources.to_data(),
            "facts": self.facts,
        })
    }
}
