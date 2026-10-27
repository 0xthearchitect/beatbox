//! Which kinds of sandbox exist here, and what each of them can do.
//!
//! The three questions everything above the runtime asks:
//!
//! ```text
//! What types are there?             -> SandboxRegistry::types
//! What can that type do?            -> SandboxType::capabilities
//! Who can run a process for me?     -> SandboxRegistry::providing
//! ```
//!
//! The third matters most: ask for `process.execute`, don't name `host`.
//!
//! No global registry and nothing registered automatically, so two runtimes
//! in one process can't surprise each other. Only a [`ProviderSpec`]
//! registers:
//!
//! ```compile_fail
//! let mut registry = beatbox_engine::SandboxRegistry::default();
//! registry.register(std::collections::HashMap::<u8, u8>::new());
//! ```

use std::collections::{BTreeMap, BTreeSet};

use beatbox_reasons::reason;
use serde_json::Value;

use crate::faults::{ContractError, EngineError, repr};
use crate::names::{namespace, segment, validate};
use crate::runtime::capabilities::CapabilitySet;
use crate::runtime::isolation::Isolation;
use crate::runtime::provider::ProviderSpec;

/// One registered kind of sandbox, as the registry publishes it.
///
/// Read off the spec at registration, so it can't disagree with the code.
#[derive(Debug, Clone)]
pub struct SandboxType {
    /// What a configuration asks for.
    pub name: String,
    /// The implementation's version.
    pub version: String,
    /// Everything it can ever do, checked.
    pub capabilities: CapabilitySet,
    /// The boundary it provides.
    pub isolation: Isolation,
    spec: ProviderSpec,
}

impl SandboxType {
    /// How it is built.
    pub fn spec(&self) -> &ProviderSpec {
        &self.spec
    }

    /// As JSON, the shape the records keep.
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "version": self.version,
            "capabilities": self.capabilities.to_list(),
            "isolation": self.isolation.as_str(),
        })
    }
}

/// What this runtime knows how to build.
#[derive(Debug, Clone, Default)]
pub struct SandboxRegistry {
    types: BTreeMap<String, SandboxType>,
}

impl SandboxRegistry {
    /// Add a type, and publish what it says about itself.
    ///
    /// A name twice is refused: taking the second silently is how a test
    /// double ends up in production. See
    /// [`SandboxRegistry::register_replacing`].
    pub fn register(&mut self, spec: ProviderSpec) -> Result<SandboxType, ContractError> {
        self.insert(spec, false)
    }

    /// Add a type, replacing one of the same name if there is one.
    pub fn register_replacing(&mut self, spec: ProviderSpec) -> Result<SandboxType, ContractError> {
        self.insert(spec, true)
    }

    fn insert(&mut self, spec: ProviderSpec, replace: bool) -> Result<SandboxType, ContractError> {
        if !segment(&spec.name) {
            return Err(ContractError::broken(format!(
                "a sandbox type's name must be one lowercase word: {}",
                repr(&spec.name)
            )));
        }
        if self.types.contains_key(&spec.name) && !replace {
            return Err(ContractError::broken(format!(
                "a sandbox type called {} is already registered",
                repr(&spec.name)
            )));
        }
        let capabilities = CapabilitySet::new(&spec.capabilities)?;
        let mut described = BTreeSet::new();
        for action in &spec.actions {
            if !capabilities.supports(&action.capability) {
                return Err(ContractError::broken(format!(
                    "{} describes {}, which it does not declare",
                    repr(&spec.name),
                    repr(&action.capability)
                )));
            }
            if !described.insert(action.capability.as_str()) {
                return Err(ContractError::broken(format!(
                    "{} describes {} twice",
                    repr(&spec.name),
                    repr(&action.capability)
                )));
            }
        }
        let mut roles = BTreeSet::new();
        for requirement in &spec.requires {
            if !segment(&requirement.role) || !roles.insert(requirement.role.as_str()) {
                return Err(ContractError::broken(format!(
                    "{} requires a role that is not one new lowercase word: {}",
                    repr(&spec.name),
                    repr(&requirement.role)
                )));
            }
            for capability in &requirement.capabilities {
                validate(capability, "a required capability")?;
                if namespace(capability) == "lifecycle" {
                    return Err(ContractError::broken(format!(
                        "{} requires {}: a peer's lifecycle is not its user's",
                        repr(&spec.name),
                        repr(capability)
                    )));
                }
            }
        }
        let entry = SandboxType {
            name: spec.name.clone(),
            version: spec.version.clone(),
            capabilities,
            isolation: spec.isolation,
            spec,
        };
        self.types.insert(entry.name.clone(), entry.clone());
        Ok(entry)
    }

    /// The type, or a refusal naming what was asked for.
    ///
    /// A refusal, not a broken contract: a record from a build that had the
    /// type, opened on one that doesn't, is ordinary.
    pub fn get(&self, name: &str) -> Result<SandboxType, EngineError> {
        self.types
            .get(name)
            .cloned()
            .ok_or_else(|| EngineError::Refused(reason!("engine.unknown_type", type = name)))
    }

    /// Every type that declares exactly `capability`, by name.
    pub fn providing(&self, capability: &str) -> Vec<SandboxType> {
        self.types
            .values()
            .filter(|entry| entry.capabilities.supports(capability))
            .cloned()
            .collect()
    }

    /// Everything registered, by name, so a listing is stable.
    pub fn types(&self) -> Vec<SandboxType> {
        self.types.values().cloned().collect()
    }

    /// The names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.types.keys().cloned().collect()
    }

    /// Whether a type of this name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.types.contains_key(name)
    }

    /// How many types.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}
