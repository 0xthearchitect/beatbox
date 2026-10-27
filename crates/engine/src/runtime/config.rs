//! What was asked for. What is true lives in
//! [`SandboxState`](crate::SandboxState); mixing `cpu: 4` with
//! `cpu_usage: 32%` in one object makes neither comparable.
//!
//! A configuration is a value: nothing edits one,
//! [`Sandbox::reconfigure`](crate::Sandbox::reconfigure) hands over a new one.
//!
//! The runtime reads the type, resources and isolation, which it acts on.
//! `settings` is the provider's, carried and never read.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::events::Data;
use crate::faults::ContractError;
use crate::runtime::identity::text_of;
use crate::runtime::isolation::IsolationPolicy;
use crate::runtime::resources::{NOTHING, Resources};

/// One sandbox, as asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxConfig {
    type_name: String,
    /// What to call it. Empty means the runtime names it after its type.
    pub name: String,
    /// What it is promised, in the ledger's units.
    pub resources: Resources,
    /// The weakest boundary it will accept.
    pub isolation: IsolationPolicy,
    /// The provider's half. Carried, never read here.
    pub settings: Data,
    /// Role to the sandbox filling it, by name or id, turned into an id once
    /// this one is created (`peers.rs`). The runtime checks it, so it reads it.
    pub uses: BTreeMap<String, String>,
}

impl SandboxConfig {
    /// A configuration for a sandbox of `type_name`, and nothing else yet.
    ///
    /// The type is the only field with no default.
    pub fn new(type_name: &str) -> Result<Self, ContractError> {
        let type_name = type_name.trim();
        if type_name.is_empty() {
            return Err(ContractError::broken(
                "a sandbox configuration has to name a type",
            ));
        }
        Ok(Self {
            type_name: type_name.to_owned(),
            name: String::new(),
            resources: NOTHING,
            isolation: IsolationPolicy::default(),
            settings: Data::new(),
            uses: BTreeMap::new(),
        })
    }

    /// The same, called `name`.
    #[must_use]
    pub fn named(mut self, name: &str) -> Self {
        self.name = name.trim().to_owned();
        self
    }

    /// The same, asking for `resources`.
    #[must_use]
    pub fn with_resources(mut self, resources: Resources) -> Self {
        self.resources = resources;
        self
    }

    /// The same, asking for at least `isolation`.
    #[must_use]
    pub fn with_isolation(mut self, isolation: IsolationPolicy) -> Self {
        self.isolation = isolation;
        self
    }

    /// The same, with `settings` for the provider.
    #[must_use]
    pub fn with_settings(mut self, settings: Data) -> Self {
        self.settings = settings;
        self
    }

    /// The same, with `target` -- a sandbox's name or id -- in `role`.
    #[must_use]
    pub fn using(mut self, role: &str, target: &str) -> Self {
        self.uses.insert(role.to_owned(), target.trim().to_owned());
        self
    }

    /// Which registered type is to be built.
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// One provider setting, if it is there.
    pub fn setting(&self, key: &str) -> Option<&Value> {
        self.settings.get(key)
    }

    /// As JSON, the shape the records keep.
    /// `uses` only when there is one, so old records read back unchanged.
    pub fn to_value(&self) -> Value {
        let mut value = serde_json::json!({
            "type": self.type_name,
            "name": self.name,
            "resources": self.resources.to_data(),
            "isolation": self.isolation.to_value(),
            "settings": self.settings,
        });
        if !self.uses.is_empty() {
            value["uses"] = serde_json::json!(self.uses);
        }
        value
    }

    /// Read back what [`SandboxConfig::to_value`] wrote. Forgiving about what
    /// is missing, strict about what is wrong: a kind of resource nobody
    /// declared, an isolation level nobody can rank, no type at all.
    pub fn from_value(raw: &Value) -> Result<Self, ContractError> {
        let text = |key: &str| raw.get(key).and_then(text_of).unwrap_or_default();
        let resources = match raw.get("resources") {
            Some(value) => Resources::from_value(value)?,
            None => NOTHING,
        };
        let isolation = match raw.get("isolation") {
            Some(value) => IsolationPolicy::from_value(value)?,
            None => IsolationPolicy::default(),
        };
        let settings = raw
            .get("settings")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut config = Self::new(&text("type"))?
            .named(&text("name"))
            .with_resources(resources)
            .with_isolation(isolation)
            .with_settings(settings);
        for (role, target) in raw
            .get("uses")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            config = config.using(role, target.as_str().unwrap_or_default());
        }
        Ok(config)
    }
}
