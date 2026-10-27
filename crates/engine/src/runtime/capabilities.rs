//! What a sandbox can do, said by the sandbox itself.
//!
//! **Capabilities are the contract, not advertising.** The runtime refuses
//! an action whose capability wasn't declared before the provider is
//! touched, so a provider can't quietly grow a power.
//!
//! `create`, `start`, `stop` and `destroy` are the floor; pause and reset are
//! optional, declared here like any other power. `lifecycle.pause` covers
//! resume too: freezing without thawing is broken, not half-capable.

use std::collections::BTreeSet;

use crate::faults::ContractError;
use crate::names::{matches, namespace, validate, validate_pattern};

/// Can be frozen and thawed: `pause` and `resume`.
pub const LIFECYCLE_PAUSE: &str = "lifecycle.pause";
/// Can be thrown back to how it was created without being destroyed first.
pub const LIFECYCLE_RESET: &str = "lifecycle.reset";
/// Can be given a different configuration while it is not running. Not a
/// lifecycle transition, but a verb a provider has to opt into all the same.
pub const LIFECYCLE_RECONFIGURE: &str = "lifecycle.reconfigure";

/// A sandbox's promises, sorted, deduplicated and checked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilitySet {
    names: Vec<String>,
}

impl CapabilitySet {
    /// These names, sorted and deduplicated, or a broken contract for the
    /// first one that is not `namespace.verb`.
    pub fn new<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Result<Self, ContractError> {
        let mut checked = BTreeSet::new();
        for name in names {
            checked.insert(validate(name.as_ref(), "a capability")?.to_owned());
        }
        Ok(Self {
            names: checked.into_iter().collect(),
        })
    }

    /// The names, sorted.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// How many.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether it can do nothing optional at all.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Exact, never a pattern: a gate taking `process.*` would open on a name
    /// nobody declared.
    pub fn supports(&self, name: &str) -> bool {
        self.names.iter().any(|declared| declared == name)
    }

    /// Every declared capability covered by `pattern`, for discovery.
    pub fn matching(&self, pattern: &str) -> Result<Vec<String>, ContractError> {
        validate_pattern(pattern, "pattern")?;
        Ok(self
            .names
            .iter()
            .filter(|name| matches(name, pattern))
            .cloned()
            .collect())
    }

    /// `process`, `filesystem`, `snapshot` -- what this sandbox is about, one
    /// level up from what it can do.
    pub fn namespaces(&self) -> Vec<String> {
        let found: BTreeSet<&str> = self.names.iter().map(|name| namespace(name)).collect();
        found.into_iter().map(str::to_owned).collect()
    }

    /// The subset a particular configuration actually supports.
    ///
    /// Only ever removes: a name the type never declared is dropped, not
    /// added, because the registry published the type's declaration.
    #[must_use]
    pub fn narrowed_to(&self, keep: &CapabilitySet) -> Self {
        Self {
            names: self
                .names
                .iter()
                .filter(|name| keep.supports(name))
                .cloned()
                .collect(),
        }
    }

    /// The names, as a list.
    pub fn to_list(&self) -> Vec<String> {
        self.names.clone()
    }
}
